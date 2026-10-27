// Real-browser proof for docs/slices/07c-UX-SHELL.md's remaining Proof
// items not already owned by dockview.spec.ts (layout/placement),
// discard.spec.ts / live-state.spec.ts (Retired view), or editor/code-panel/
// terminal specs (panel-specific behavior): the flattened Project→Socket
// sidebar, and the compact "Configure agent" card that replaces the chat
// surface when a fresh socket has no ready agent. Same harness as
// editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, seatAssign } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

test.describe("07c-UX-SHELL: sidebar hierarchy and first-use chat", () => {
  test("sidebar renders Project → Socket directly, with no intermediate branch level", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-uxshell-flat-${Date.now()}`;
    await openStream(request, repo, name);

    await page.goto("/w");

    // The socket row is reachable directly under its (auto-expanded)
    // project row — no branch row to expand in between.
    await expect(page.locator(".session-row", { hasText: name })).toBeVisible({ timeout: 15_000 });
    // The old branch level's markup does not exist anywhere in the tree.
    await expect(page.locator(".branch-row")).toHaveCount(0);
    await expect(page.locator(".branch-track")).toHaveCount(0);
  });

  test("a socket's row title is not duplicated by its own secondary line", async ({ page, request }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-uxshell-label-${Date.now()}`;
    await openStream(request, repo, name);

    await page.goto("/w");

    const row = page.locator(".session-row", { hasText: name });
    await expect(row).toBeVisible({ timeout: 15_000 });
    // The row title is the socket's chosen name; the branch — identical to
    // the auto-generated name for a fresh `/project/open` call here — must
    // not also print as a redundant secondary fact underneath it.
    await expect(row.locator(".sess-status-str", { hasText: name })).toHaveCount(0);
  });

  test("a fresh socket with no agent shows a compact Configure agent card, not an empty dock", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-uxshell-noagent-${Date.now()}`);

    await gotoStudio(page, streamId);
    // Alone, the conversation shows no tab strip: its tab is in the DOM, hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    // A freshly opened stream has no seat assignment yet — the compact card
    // is what fills the dominant surface, not an empty or full chat log.
    const card = page.locator('[data-testid="configure-card"]');
    await expect(card).toBeVisible({ timeout: 15_000 });
    await expect(card.getByText("No agent configured for Orchestrator yet.")).toBeVisible();
    await expect(page.locator('[data-testid="transcript"]')).toHaveCount(0);
    await expect(page.locator('[data-testid="composer"]')).toHaveCount(0);

    // The card's own controls are live, not inert: every declared role has
    // an assign control wired to a real model list from the socket
    // projection (disabled only when there is genuinely nothing to pick).
    await expect(card.locator('[data-testid="seat-row"]').first()).toBeVisible();
    await expect(card.locator('[data-testid="assign"]').first()).toBeVisible();
  });

  test("Chat's history never repeats the project/branch the title bar already shows", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const name = `e2e-uxshell-nodup-${Date.now()}`;
    const streamId = await openStream(request, repo, name);

    // Any assignment (readiness doesn't matter here) makes `hasAnyAssignment`
    // true, so the full log renders instead of the Configure-agent card —
    // that log, seeded by this socket's own `project.onboarded` history
    // event, is what's under test.
    await seatAssign(request, streamId, "orchestrator", "claude");

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });

    const chatLog =page.locator('[data-testid="transcript"]');
    await expect(chatLog).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="configure-card"]')).toHaveCount(0);

    // The title bar names the chat and its branch once (verified by the
    // title-bar specs elsewhere); the log itself — which includes the
    // onboarding event in its underlying data — must print no second copy.
    await expect(chatLog.getByText(name)).toHaveCount(0);
    await expect(page.locator(".session-rule")).toHaveCount(0);
  });

  test("assigning a working agent switches the compact card to the full conversation, live", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-uxshell-live-assign-${Date.now()}`);

    // This e2e daemon has no real provider credentials configured, so a real
    // assignment never becomes socket_ready — fake readiness at the wire
    // level instead, the same technique live-state.spec.ts's stale-response
    // test uses. `claudeReady` makes the picker offer a model to assign
    // *before* the real POST lands; once it lands, the orchestrator agent
    // itself flips to assigned + ready, exactly as a working provider would
    // report back through the next `/studio/state` poll.
    let assigned = false;
    await page.route("**/seat/assign", async (route) => {
      assigned = true;
      await route.continue();
    });
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
        const claudeReady = s.available_models.find((m: { id: string }) => m.id === "claude");
        if (claudeReady) {
          claudeReady.ready = true;
          claudeReady.reason = undefined;
        }
        if (assigned) {
          const orch = s.agents.find((a: { role_id: string }) => a.role_id === "orchestrator");
          if (orch) {
            orch.assigned_model_id = "claude";
            orch.socket_ready = true;
            orch.reason = undefined;
          }
        }
      }
      await route.fulfill({ response, body: JSON.stringify(body) });
    });

    await gotoStudio(page, streamId);
    const card = page.locator('[data-testid="configure-card"]');
    await expect(card).toBeVisible({ timeout: 15_000 });

    const orchestratorRow = card.locator('[data-testid="seat-row"]').first();
    await orchestratorRow.locator('[data-testid="assign-model-select"]').selectOption({ label: "Claude (Anthropic)" });
    await orchestratorRow.locator('[data-testid="assign"]').click();

    // The card is gone and the full conversation is showing — reactively,
    // with no reload and no manual re-navigation.
    await expect(page.locator('[data-testid="configure-card"]')).toHaveCount(0, { timeout: 15_000 });
    await expect(page.locator('[data-testid="transcript"]')).toBeVisible();
    await expect(page.locator('[data-testid="composer"]')).toBeVisible();
  });
});
