// Real-browser proof for 07d-A4: the per-socket Review pane renders a
// GitHub-style split diff (Monaco DiffEditor) for a pending proposal and
// Accept/Reject drive the real client decide path. This e2e daemon has no
// provider credentials, and `resolve_seat_model("stub")` is `#[cfg(test)]`
// gated out of the production binary Playwright spawns (STEERING.md §
// Models: "auto never silently becomes stub"), so no HTTP path here can
// produce a real proposal. Per the slice's own Proof line ("propose (or
// fixture pending event)"), the pending proposal and its decision are
// injected at the network boundary via `page.route` — the same pattern
// `fakeAgentProjection` already uses in helpers.ts to work around the same
// no-credentials gap. Backend accept/reject correctness itself (the disk
// write, the digest/causality bookkeeping) is proven separately and
// exhaustively by `cargo test -p bakery-node`; this spec's job is the
// client wiring: split diff renders, agent identity/rationale show, and a
// click really calls `/agent/accept` or `/agent/reject` with the right body
// and folds the result back into the same card.
import { expect, type Page, type WebSocketRoute } from "@playwright/test";
import { test } from "./fixtures";
import { fakeEvent, makeScratchRepo, openReviewPanel, openStream, toHandle, AUTH_HEADERS, NODE_BASE } from "./helpers";

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

/** Intercepts one decide call (`/agent/accept` or `/agent/reject`) and
 *  fulfills it with a synthetic decision event, capturing the request body
 *  the client actually sent. */
async function stubDecide(
  page: Page,
  kind: "accept" | "reject",
  streamId: string,
  eventHash: string,
  filePath: string,
): Promise<{ body(): unknown }> {
  let captured: unknown = null;
  await page.route(`**/agent/${kind}`, async (route) => {
    captured = route.request().postDataJSON();
    const decisionEvent = fakeEvent(
      streamId,
      kind === "accept" ? "agent.edit.accepted" : "agent.edit.rejected",
      { decided_event: eventHash, file_path: filePath, status: kind === "accept" ? "accepted" : "rejected" },
      { actor: "human", causality: [eventHash] },
    );
    await route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(decisionEvent) });
  });
  return { body: () => captured };
}

test.describe("Review pane split diff (07d-A4)", () => {
  test("live changes wait in the chat; Review opens only when asked, on the change asked for", async ({ page, request }) => {
    const repo = makeScratchRepo({ "hello.txt": "old" });
    const streamId = await openStream(request, repo, `live-review-${Date.now()}`);
    let socket: WebSocketRoute | undefined;
    await page.routeWebSocket(/\/events(\?|$)/, route => { route.connectToServer(); socket = route; });
    await gotoStudio(page, streamId);
    // Attached, not visible: a lone panel's tab strip is hidden.
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached();
    await expect.poll(() => !!socket).toBe(true);
    const proposal = (hash: string) => fakeEvent(streamId, "agent.file.changed", {
      file_path: "hello.txt", old_content: "old", new_content: hash, status: "pending", agent_id: `${streamId}:orchestrator`,
    }, { event_hash: hash });
    const changes = page.locator('[data-testid="agent-panel"] [data-testid="proposal"]');
    const reviewTab = page.locator(".dv-default-tab", { hasText: "Review queue" });
    socket!.send(JSON.stringify(proposal("first-live")));
    await expect(changes).toHaveCount(1);
    socket!.send(JSON.stringify(proposal("second-live")));
    await expect(changes).toHaveCount(2);
    await expect(reviewTab).toHaveCount(0);
    // The older change's own button opens Review on it — not on the newest,
    // which is Review's default focus (the list is newest first).
    await changes.first().getByRole("button", { name: "Open in Review" }).click();
    await expect(reviewTab).toBeVisible();
    await expect(page.locator(".review")).toContainText("2 waiting on you");
    const rows = page.locator(".list .row");
    await expect(rows.nth(1)).toHaveClass(/\bactive\b/);
    await changes.last().getByRole("button", { name: "Open in Review" }).click();
    await expect(rows.nth(0)).toHaveClass(/\bactive\b/);
  });

  test("permanent Baker has the same docked live Review surface", async ({ page, request }) => {
    const response = await request.get(`${NODE_BASE}/baker`, { headers: AUTH_HEADERS });
    expect(response.ok()).toBeTruthy();
    const home = await response.json();
    await seedHistory(page, home.stream_id, [fakeEvent(home.stream_id, "agent.file.changed", {
      file_path: "theme.ts", old_content: "before", new_content: "after", status: "pending", agent_id: home.agent_id,
    })]);
    await page.goto("/baker");
    await expect(page.locator(".dv-default-tab", { hasText: /^Baker$/ })).toBeAttached();
    await page.locator('[data-testid="agent-panel"]').getByRole("button", { name: "Open in Review" }).first().click();
    await expect(page.locator(".review")).toBeVisible();
    await expect(page.getByRole("button", { name: "Edit then accept", exact: true })).toBeEnabled();
  });

  test("pending review opens beside chat and edited acceptance keeps its draft after a failure", async ({ page, request }) => {
    const repo = makeScratchRepo({ "hello.txt": "original\n" });
    const streamId = await openStream(request, repo, `edit-review-${Date.now()}`);
    const hash = "editable-proposal";
    await seedHistory(page, streamId, [fakeEvent(streamId, "agent.file.changed", {
      file_path: "hello.txt", status: "pending", old_content: "original\n",
      new_content: "agent proposal\n", agent_id: `${streamId}:orchestrator`,
    }, { event_hash: hash })]);
    let captured: Record<string, unknown> | null = null;
    let attempts = 0;
    await page.route("**/agent/accept", async route => {
      captured = route.request().postDataJSON();
      if (++attempts === 1) {
        await route.fulfill({ status: 500, body: "fixture: retry this decision" });
        return;
      }
      await route.fulfill({ json: fakeEvent(streamId, "agent.edit.accepted", {
        decided_event: hash, file_path: "hello.txt", status: "accepted",
        human_edited: true, new_content: captured!.new_content,
      }, { actor: "human", causality: [hash] }) });
    });
    await gotoStudio(page, streamId);
    // Pending history does not open Review; the change's own button does.
    await page.locator('[data-testid="agent-panel"]').getByRole("button", { name: "Open in Review" }).first().click();
    await expect(page.locator(".review")).toBeVisible();
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeVisible();
    await page.getByRole("button", { name: "Edit then accept", exact: true }).click();
    await expect(page.getByRole("button", { name: "Accept edited content · write to disk", exact: true })).toBeVisible();
    await page.keyboard.press("ControlOrMeta+a");
    await page.keyboard.insertText("human reviewed revision\n");
    await expect(page.locator(".diff-host .editor.modified")).toContainText("human reviewed revision");
    await page.getByRole("button", { name: "Accept edited content · write to disk", exact: true }).click();
    await expect(page.locator(".review [role=alert]")).toContainText("retry this decision");
    await expect(page.locator(".diff-host .editor.modified")).toContainText("human reviewed revision");
    await page.getByRole("button", { name: "Accept edited content · write to disk", exact: true }).click();
    await expect(page.locator(".focus .chip-ok")).toHaveText("accepted");
    expect(captured).toEqual({ stream_id: streamId, event_hash: hash, new_content: "human reviewed revision\n" });
    // Accepted display comes from the decision, not the original proposal.
    await expect(page.locator(".diff-host .editor.modified")).toContainText("human reviewed revision");
  });

  test("a pending proposal shows split original/current, then Accept and Reject each decide correctly", async ({
    page,
    request,
  }) => {
    const repo = makeScratchRepo({ "hello.txt": "line one\nold middle\nline three\n" });
    const streamId = await openStream(request, repo, `e2e-review-${Date.now()}`);

    const acceptHash = "fixture-accept-1";
    const rejectHash = "fixture-reject-1";
    const acceptProposal = fakeEvent(
      streamId,
      "agent.file.changed",
      {
        file_path: "hello.txt",
        status: "pending",
        rationale: "tidy the greeting",
        old_content: "line one\nold middle\nline three\n",
        new_content: "line one\nnew middle\nline three\nline four\n",
        agent_id: `${streamId}:worker`,
        model_id: "grok",
      },
      { event_hash: acceptHash },
    );
    const rejectProposal = fakeEvent(
      streamId,
      "agent.file.changed",
      {
        file_path: "bye.txt",
        status: "pending",
        rationale: "add a farewell line",
        old_content: "keep\n",
        new_content: "keep\nadded by agent\n",
        agent_id: `${streamId}:worker`,
        model_id: "grok",
      },
      { event_hash: rejectHash },
    );
    await seedHistory(page, streamId, [acceptProposal, rejectProposal]);

    await gotoStudio(page, streamId);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openReviewPanel(page);

    await expect(page.getByText("2 waiting on you")).toBeVisible({ timeout: 15_000 });

    // Focus defaults to the top of the queue (newest pending first) — that's
    // the farewell-line proposal (rejectProposal was seeded second).
    await expect(page.locator(".focus .path", { hasText: "bye.txt" })).toBeVisible();
    await expect(page.locator(".author", { hasText: `${streamId}:worker` })).toBeVisible();
    await expect(page.locator(".author", { hasText: "grok" })).toBeVisible();
    await expect(page.locator(".rationale", { hasText: "add a farewell line" })).toBeVisible();

    // Pick the other proposal from the list — one focus at a time.
    await page.locator(".list .row", { hasText: "hello.txt" }).click();
    await expect(page.locator(".focus .path", { hasText: "hello.txt" })).toBeVisible();
    await expect(page.locator(".rationale", { hasText: "tidy the greeting" })).toBeVisible();

    // GitHub-style split diff: a real Monaco DiffEditor, both sides visible —
    // the deleted line on one side, the two added lines on the other.
    const diffEditor = page.locator(".diff-host .monaco-diff-editor");
    await expect(diffEditor).toBeVisible();
    await expect(diffEditor.locator(".view-line", { hasText: "old middle" })).toBeVisible();
    await expect(diffEditor.locator(".view-line", { hasText: "new middle" })).toBeVisible();
    await expect(diffEditor.locator(".view-line", { hasText: "line four" })).toBeVisible();

    // Accept the focused (hello.txt) proposal — real client → real request →
    // fixture response folds back into the same card as "accepted".
    const accept = await stubDecide(page, "accept", streamId, acceptHash, "hello.txt");
    await page.getByRole("button", { name: "Accept · write to disk" }).click();
    await expect.poll(() => accept.body()).toEqual({ stream_id: streamId, event_hash: acceptHash });
    await expect(page.locator(".row.accepted", { hasText: "hello.txt" })).toBeVisible();
    await expect(page.locator(".focus .chip-ok", { hasText: "accepted" })).toBeVisible();

    // Reject the other one — no write, card goes rejected.
    await page.locator(".list .row", { hasText: "bye.txt" }).click();
    const reject = await stubDecide(page, "reject", streamId, rejectHash, "bye.txt");
    await page.getByRole("button", { name: "Reject", exact: true }).click();
    await expect.poll(() => reject.body()).toEqual({ stream_id: streamId, event_hash: rejectHash });
    await expect(page.locator(".row.rejected", { hasText: "bye.txt" })).toBeVisible();

    await expect(page.getByText("all reviewed")).toBeVisible();
  });

  test("a pending proposal on one socket does not appear in another socket's Review pane", async ({
    page,
    request,
  }) => {
    const repoA = makeScratchRepo({ "a.txt": "a\n" });
    const repoB = makeScratchRepo({ "b.txt": "b\n" });
    const streamA = await openStream(request, repoA, `e2e-review-a-${Date.now()}`);
    const streamB = await openStream(request, repoB, `e2e-review-b-${Date.now()}`);

    const proposalA = fakeEvent(streamA, "agent.file.changed", {
      file_path: "a.txt",
      status: "pending",
      rationale: "socket A only",
      old_content: "a\n",
      new_content: "a\na2\n",
      agent_id: `${streamA}:worker`,
    });
    await seedHistory(page, streamA, [proposalA]);

    await gotoStudio(page, streamB);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openReviewPanel(page);
    await expect(page.getByText("no proposals yet")).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText("a.txt")).toHaveCount(0);

    await gotoStudio(page, streamA);
    await expect(page.locator(".dv-default-tab", { hasText: "Orchestrator" })).toBeAttached({ timeout: 15_000 });
    await openReviewPanel(page);
    await expect(page.locator(".list .row", { hasText: "a.txt" })).toBeVisible({ timeout: 15_000 });
  });
});
