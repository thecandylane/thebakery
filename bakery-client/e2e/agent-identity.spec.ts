// Real-browser proof for docs/slices/07d-A-AGENT-IDENTITY.md's Proof
// checklist (items 1-4). This e2e daemon has no real provider credentials
// configured, so a turn can never actually complete here — Rust-level tests
// in crates/bakery-node/src/routes/agent.rs already cover the server-side
// substance directly (agent_id stability across a model reassignment,
// prompt isolation between two agents, and genuine non-blocking
// concurrency via a gated model + threads). These specs instead prove what
// only a real browser can: that the CLIENT correctly binds one Chat pane
// per agent, keeps their state independent, survives pane close/reopen,
// and rehydrates the right binding after a reload — faking readiness via
// route interception where needed, the same technique the existing
// ux-shell.spec.ts assignment-transition test uses. Same harness as
// editor.spec.ts (see playwright.config.ts / helpers.ts).
import { expect, type Page } from "@playwright/test";
import { test } from "./fixtures";
import { makeScratchRepo, openStream, toHandle, seatAssign, fakeAgentProjection, openMemoryPanel } from "./helpers";

async function gotoStudio(page: Page, streamId: string) {
  await page.goto(`/w/${toHandle(streamId)}`);
}

/** Opens `roleLabel`'s pane through the real "+ Panel" menu. A "Conversations"
 *  row (already assigned) opens the pane directly and the menu closes. An
 *  "Add agent" row (unassigned) instead expands an inline model picker in
 *  the menu itself (07d-A1 §4: "Assign first, then open that agent's
 *  conversation") — `modelLabel` is only used in that case. */
async function openConversation(page: Page, roleLabel: string, modelLabel = "Grok (xAI)") {
  await page.getByRole("button", { name: "+ Panel" }).click();
  const menu = page.locator(".menu");
  await menu.getByRole("button", { name: roleLabel, exact: true }).click();
  const inline = menu.locator(".add-agent-inline");
  try {
    await inline.waitFor({ state: "visible", timeout: 800 });
    await inline.locator(".model-select").selectOption({ label: modelLabel });
    await inline.locator(".assign-btn").click();
  } catch {
    // A "Conversations" row: the pane already opened and the menu closed.
  }
}

test.describe("07d-A-AGENT-IDENTITY", () => {
  // ---- 1. Model reassignment keeps the same agent/pane, not a new one ----

  test("reassigning orchestrator's model keeps the same Chat pane — no new tab, no title change", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-aid-swap-${Date.now()}`);

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(orchTab).toBeAttached({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab")).toHaveCount(1);

    await seatAssign(request, streamId, "orchestrator", "grok");
    await seatAssign(request, streamId, "orchestrator", "claude"); // reassignment, same seat

    // Still exactly one tab, still titled "Orchestrator" — reassigning the
    // engine never mints a second pane or renames this one to the model.
    await expect(page.locator(".dv-default-tab")).toHaveCount(1);
    await expect(orchTab).toBeAttached();
    await expect(page.locator('[data-testid="agent-role"]', { hasText: "Orchestrator" })).toBeVisible();
  });

  // ---- 2. Two configured agents, independent state, not mixed -----------

  test("orchestrator and worker are two independent panes — one agent's readiness never affects the other", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-aid-two-${Date.now()}`);

    await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
    // A real assignment this e2e daemon has no credentials for — worker
    // carries its own, independently un-ready state (07d-A1 §4 moved
    // unassigned roles to an inline "Add agent" picker in the menu, so a
    // still-unassigned role can no longer be "opened" directly; a real,
    // differently-unready assignment proves the same independence claim).
    await seatAssign(request, streamId, "worker", "claude");

    await gotoStudio(page, streamId);
    // Attached, not visible: the tab strip shows only once Worker's pane
    // makes a second panel.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openConversation(page, "Worker");
    await expect(page.locator(".dv-default-tab", { hasText: "Worker" })).toBeVisible();

    // Both panes are real (07d-A "always render" — kept alive off-screen),
    // both present in the DOM at once.
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(2);

    // Orchestrator (faked ready) shows the full conversation; Worker (a
    // real assignment with no live credentials here) shows its own
    // Setup-needed notice, not a hollow configure card — orchestrator's
    // readiness must not leak into worker's pane.
    await expect(page.locator('[data-testid="configure-card"]')).toHaveCount(0);
    const workerChat = page
      .locator('[data-testid="agent-panel"]')
      .filter({ has: page.locator('[data-testid="agent-role"]', { hasText: "Worker" }) });
    await expect(workerChat.locator('[data-testid="agent-unavailable"]')).toContainText("Setup needed");
    await expect(page.locator('[data-testid="composer"]')).toHaveCount(2);
  });

  // ---- 3. Closing a pane never resets its agent's state ------------------

  test("closing a busy pane and reopening it still shows busy — the conversation outlives the pane", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-aid-busy-${Date.now()}`);

    await fakeAgentProjection(page, streamId, "orchestrator", {
      assigned_model_id: "grok",
      socket_ready: true,
      busy: true,
    });

    await gotoStudio(page, streamId);
    const orchTab = page.locator(".dv-default-tab", { hasText: "Orchestrator" });
    await expect(orchTab).toBeAttached({ timeout: 15_000 });
    await expect(page.locator('[data-testid="activity"]')).toBeVisible();
    await expect(page.locator('[data-testid="send"]')).toBeDisabled();

    // Close the dock tab — a view close, not a cancel. A lone panel has no
    // tab strip, so open Memory & context first to show Orchestrator's tab
    // and its close button. Memory stays open, and the reopened Orchestrator
    // pane joins Memory's group as its front tab.
    await openMemoryPanel(page);
    await expect(orchTab).toBeVisible();
    await orchTab.locator(".dv-default-tab-action").click();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(0);

    // Reopen through the real "+ Panel" menu — busy is still true, read
    // straight from `/studio/state` again, not reset to idle by the pane
    // having been gone.
    await openConversation(page, "Orchestrator");
    await expect(page.locator('[data-testid="activity"]')).toBeVisible({ timeout: 15_000 });
    await expect(page.locator('[data-testid="send"]')).toBeDisabled();
  });

  // ---- 4. Reload rehydrates each pane to its own correct agent -----------

  test("reloading restores both panes bound to their own agent, not swapped or merged", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "f.txt": "hi\n" });
    const streamId = await openStream(request, repo, `e2e-aid-reload-${Date.now()}`);
    // Assigned before opening — 07d-A1 §4 moved unassigned roles to an
    // inline "Add agent" picker in the menu, so this test's own substance
    // (reload rebinds each restored panel to the right agent, never
    // swapped) no longer needs the open-then-assign sequencing to prove it.
    await seatAssign(request, streamId, "worker", "claude");

    await gotoStudio(page, streamId);
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openConversation(page, "Worker");
    await expect(page.locator(".dv-default-tab", { hasText: "Worker" })).toBeVisible();

    // Give the debounced persist (300ms) time to land before reloading.
    await page.waitForTimeout(500);
    await page.reload();

    // The restored layout has both panes, so the tab strip shows.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible({ timeout: 15_000 });
    await expect(page.locator(".dv-default-tab", { hasText: "Worker" })).toBeVisible();
    await expect(page.locator('[data-testid="agent-panel"]')).toHaveCount(2);

    // Bring worker's own tab to front, then confirm its pane — not
    // orchestrator's — shows worker's own assignment: proof the restored
    // dockview panel params (agentId/roleId) rebound to the correct
    // AgentConversation after the reload, not swapped or merged.
    await page.locator(".dv-default-tab", { hasText: "Worker" }).click();
    const workerChat = page
      .locator('[data-testid="agent-panel"]')
      .filter({ has: page.locator('[data-testid="agent-role"]', { hasText: "Worker" }) });
    await expect(workerChat).toHaveCount(1);
    // Identity now lives in the pane header rather than behind a disclosure:
    // the role and its own assignment are both visible without a click.
    await expect(workerChat.locator('[data-testid="agent-role"]')).toHaveText("Worker");
    await expect(workerChat.locator('[data-testid="agent-model"]')).toHaveText("claude");
  });
});
