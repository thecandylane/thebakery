// The chat follows its output. Measured in WebKit (the desktop app's
// engine), where the gap showed: when a command finished, its block folded
// while the live strip appeared, and the transcript stopped 79px short of
// the bottom until unrelated content arrived. Chromium never showed it.
import { expect, type Page, type WebSocketRoute } from "@playwright/test";
import { test } from "./fixtures";
import { fakeEvent, makeScratchRepo, openStream, seatAssign, toHandle } from "./helpers";

test.use({ browserName: "webkit" });

/** Pixels between the transcript's bottom edge and the bottom of its scroller. */
async function gap(page: Page): Promise<number> {
  return page.evaluate(() => {
    let el = document.querySelector<HTMLElement>('[data-testid="transcript"]');
    while (el && !/(auto|scroll)/.test(getComputedStyle(el).overflowY)) el = el.parentElement;
    if (!el) throw new Error("no scroller around the transcript");
    return Math.round(el.scrollHeight - el.clientHeight - el.scrollTop);
  });
}

test("the chat stays at the bottom as output lands, and leaves a reader who scrolled up alone", async ({
  page,
  request,
}) => {
  const streamId = await openStream(request, makeScratchRepo({ "f.txt": "hi\n" }), `e2e-scroll-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  const agentId = `${streamId}:orchestrator`;
  const para = (i: number) => `Paragraph ${i}. ` + "The quick brown fox jumps over the lazy dog. ".repeat(6);

  let ws: WebSocketRoute | undefined;
  await page.routeWebSocket(/\/events(\?|$)/, (route) => {
    route.connectToServer();
    ws = route;
  });
  const send = (family: string, payload: Record<string, unknown>) =>
    ws!.send(JSON.stringify(fakeEvent(streamId, family, { agent_id: agentId, ...payload })));
  const activity = (phase: Record<string, unknown>) =>
    ws!.send(
      JSON.stringify({
        kind: "activity",
        activity: {
          agent_id: agentId,
          stream_id: streamId,
          turn_id: "turn-e2e",
          turn_started_at: new Date().toISOString(),
          recent_completed_tools: [],
          usage: null,
          cancel_requested: false,
          phase,
        },
      }),
    );
  const terminal = (status: string, meta: Record<string, unknown>) =>
    send("agent.acp.tool_call", {
      event: "tool_call_update",
      detail: { toolCallId: "tc-e2e", status, _meta: meta },
    });

  await page.goto(`/chat/${toHandle(streamId)}`);
  await expect(page.getByTestId("transcript")).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => ws !== undefined).toBe(true);

  send("agent.turn.started", { instruction: "run the build and tell me what happens" });
  for (let i = 0; i < 14; i++) send("agent.acp.text", { text: para(i) });
  await expect.poll(() => gap(page)).toBeLessThanOrEqual(2);

  // A command streams its output, then finishes as the reply starts.
  activity({ phase: "running_tool", tool: "Bash", call_id: "tc-e2e", started_at: new Date().toISOString(), kind: "tool", preview: "pnpm check:all" });
  send("agent.acp.tool_call", {
    event: "tool_call",
    detail: { toolCallId: "tc-e2e", title: "pnpm check:all", kind: "execute", status: "in_progress", _meta: { terminal_info: { terminal_id: "t" } } },
  });
  for (let i = 0; i < 12; i++) {
    terminal("in_progress", { terminal_output_delta: { terminal_id: "t", data: `building step ${i}\n`.repeat(4) } });
    await page.waitForTimeout(100);
  }
  await expect.poll(() => gap(page)).toBeLessThanOrEqual(2);
  terminal("completed", { terminal_exit: { terminal_id: "t", exit_code: 0 } });
  activity({ phase: "streaming", text: `Done. ${para(99)}` });
  await page.waitForTimeout(600);
  expect(await gap(page)).toBeLessThanOrEqual(2);

  // A reader who scrolled up keeps their place while output keeps landing.
  const centre = await page.evaluate(() => {
    let el = document.querySelector<HTMLElement>('[data-testid="transcript"]');
    while (el && !/(auto|scroll)/.test(getComputedStyle(el).overflowY)) el = el.parentElement;
    const r = el!.getBoundingClientRect();
    return { x: r.left + r.width / 2, y: r.top + r.height / 2 };
  });
  await page.mouse.move(centre.x, centre.y);
  await page.mouse.wheel(0, -700);
  await page.waitForTimeout(400);
  const away = await gap(page);
  expect(away).toBeGreaterThan(200);
  for (let i = 30; i < 34; i++) send("agent.acp.text", { text: para(i) });
  await page.waitForTimeout(800);
  expect(await gap(page)).toBeGreaterThanOrEqual(away);
});
