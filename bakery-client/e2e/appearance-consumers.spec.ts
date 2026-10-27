// Renderer proof, complementary to the Appearance form tests. These use the
// real shared settings API (no synthetic event) with already-open Monaco/xterm
// instances. The normal isolated runner owns the node, PTY and scratch files.
import { expect } from "@playwright/test";
import { test } from "./fixtures";
import { fakeAgentProjection, fakeEvent, makeScratchRepo, openCodePanel, openStream, seatAssign, toHandle } from "./helpers";

test.afterEach(async ({ page }) => {
  await page.unrouteAll({ behavior: "ignoreErrors" });
});

test("saving the same custom scheme restyles live editor and terminal without recreating them", async ({ page, request }) => {
  test.setTimeout(45_000);
  const repo = makeScratchRepo({ "palette.ts": "const answer = 42;\n" });
  const streamId = await openStream(request, repo, `e2e-appearance-consumers-${Date.now()}`);
  await page.goto(`/w/${toHandle(streamId)}`);
  await openCodePanel(page);
  await page.locator(".row.file", { hasText: "palette.ts" }).click();
  const editor = page.locator(".monaco-host .monaco-editor");
  await expect(editor).toBeVisible();
  const editorNode = await editor.elementHandle();

  await page.getByRole("button", { name: "+ Panel" }).click();
  await page.getByRole("button", { name: "New host terminal", exact: true }).click();
  const terminal = page.locator(".terminal-widget");
  await expect(terminal.getByText("live", { exact: true })).toBeVisible({ timeout: 15_000 });
  const terminalNode = await terminal.locator(".xterm").elementHandle();
  await terminal.locator(".term-shell").click();
  await page.keyboard.type("printf '\\033[31mPALETTE_RED\\033[0m\\n'");
  await page.keyboard.press("Enter");
  const ansiRed = terminal.locator(".xterm-fg-1").filter({ hasText: "PALETTE_RED" });
  await expect(ansiRed).toBeVisible();

  // Vite serves the SAME module instance the app imports. This calls normal
  // persistence+apply, not a test-only cache, injected stylesheet or event.
  const schemeId = await page.evaluate(async () => {
    const modulePath = "/src/lib/theme.ts";
    const { saveScheme } = await import(/* @vite-ignore */ modulePath);
    return saveScheme({
      name: "Live renderer proof", base: "dark",
      tokens: {
        "--bg-app": "rgb(17, 34, 51)", "--bg-rail": "#234",
        "--fg-body": "#e0e1e2", "--bad": "hsl(0, 100%, 60%)",
        "--syntax-keyword": "rgba(34, 170, 85, .6)",
      },
    }).id as string;
  });
  const editorBackground = page.locator(".monaco-host .monaco-editor-background").first();
  const keyword = page.locator(".monaco-host .view-line span").filter({ hasText: /^const$/ });
  const terminalBackground = terminal.locator(".xterm-scrollable-element");
  const terminalRows = terminal.locator(".xterm-rows");
  await expect(editorBackground).toHaveCSS("background-color", "rgb(17, 34, 51)");
  await expect(keyword).toHaveCSS("color", "rgb(34, 170, 85)");
  await expect(terminalBackground).toHaveCSS("background-color", "rgb(34, 51, 68)");
  await expect(terminalRows).toHaveCSS("color", "rgb(224, 225, 226)");
  await expect(ansiRed).toHaveCSS("color", "rgb(255, 51, 51)");
  await expect(editor).toHaveClass(/\bvs-dark\b/);
  await expect(page.locator("html")).not.toHaveAttribute("data-theme");

  // Same ID, same base/data-theme: only tokens change. An attribute-change-only
  // notifier misses this update and leaves both non-CSS renderers stale.
  await page.evaluate(async (id) => {
    const modulePath = "/src/lib/theme.ts";
    const { saveScheme } = await import(/* @vite-ignore */ modulePath);
    saveScheme({
      id, name: "Live renderer proof", base: "dark",
      tokens: {
        "--bg-app": "hsl(0, 0%, 98%)", "--bg-rail": "#f0f1f2",
        "--fg-body": "rgb(24, 25, 26)", "--bad": "rgb(128, 34, 68)",
        "--syntax-keyword": "#50607088",
      },
    });
  }, schemeId);
  await expect(page.locator("html")).toHaveAttribute("data-color-scheme", schemeId);
  await expect(page.locator("html")).not.toHaveAttribute("data-theme");
  await expect(editorBackground).toHaveCSS("background-color", "rgb(250, 250, 250)");
  await expect(keyword).toHaveCSS("color", "rgb(80, 96, 112)");
  await expect(terminalBackground).toHaveCSS("background-color", "rgb(240, 241, 242)");
  await expect(terminalRows).toHaveCSS("color", "rgb(24, 25, 26)");
  await expect(ansiRed).toHaveCSS("color", "rgb(128, 34, 68)");
  // This is Monaco's inherited renderer class, not just our CSS surface token.
  await expect(editor).toHaveClass(/\bvs\b/);
  await expect(editor).not.toHaveClass(/\bvs-dark\b/);
  await expect(page.locator("html")).toHaveCSS("color-scheme", "light");
  expect(await editorNode!.evaluate((element) => element.isConnected)).toBe(true);
  expect(await terminalNode!.evaluate((element) => element.isConnected)).toBe(true);

  // A built-in light base also clears the custom overrides on those same
  // instances. Resolve its CSS once rather than duplicate preset literals.
  const light = await page.evaluate(async () => {
    const modulePath = "/src/lib/theme.ts";
    const { applyTheme } = await import(/* @vite-ignore */ modulePath);
    const colorsPath = "/src/lib/themeColors.ts";
    const { colorToHex } = await import(/* @vite-ignore */ colorsPath);
    applyTheme("light");
    const css = getComputedStyle(document.documentElement);
    const rgb = (key: string) => {
      const hex = colorToHex(css.getPropertyValue(key));
      return `rgb(${[1, 3, 5].map((offset) => parseInt(hex.slice(offset, offset + 2), 16)).join(", ")})`;
    };
    return { editor: rgb("--bg-app"), terminal: rgb("--bg-rail") };
  });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(editor).toHaveClass(/\bvs\b/);
  await expect(editorBackground).toHaveCSS("background-color", light.editor);
  await expect(terminalBackground).toHaveCSS("background-color", light.terminal);
  await expect(ansiRed).toBeVisible();
});

test("chat and Baker obey the selected palette even when the operating system chooses the opposite mode", async ({ page, request }) => {
  const repo = makeScratchRepo({ "palette.txt": "theme fixture\n" });
  const streamId = await openStream(request, repo, `e2e-appearance-chat-${Date.now()}`);
  await seatAssign(request, streamId, "orchestrator", "grok");
  await fakeAgentProjection(page, streamId, "orchestrator", { assigned_model_id: "grok", socket_ready: true });
  // Only conversation content is a fixture; colors/React/markdown are the real
  // rendered app. No provider/account compatibility is claimed by this proof.
  // Fixed hash: history is refetched when the socket opens and deduped by
  // event_hash, so a fresh hash per fetch would render the reply twice.
  await page.route(`**/stream/${encodeURIComponent(streamId)}`, async (route) => {
    const response = await route.fetch();
    const body = await response.json();
    body.push(fakeEvent(streamId, "agent.turn.done", {
      agent_id: `${streamId}:orchestrator`, turn_id: "palette-reply",
      summary: "Palette proof\n\n```typescript\nconst bakery = true;\n```",
    }, { event_hash: "fake-hash-palette-reply" }));
    await route.fulfill({ response, body: JSON.stringify(body) });
  });
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto(`/w/${toHandle(streamId)}`);
  const panel = page.locator('[data-testid="agent-panel"]');
  const prose = panel.locator(".bakery-ai-prose");
  // The code surface is the block itself (--bg-app, the code background);
  // its body is transparent over it.
  const block = prose.locator('[data-streamdown="code-block"]');
  const code = block.locator('[data-streamdown="code-block-body"]');
  const model = panel.locator('[data-testid="assign-model"]');
  await expect(prose).toContainText("Palette proof");
  await expect(code).toContainText("const bakery = true;");

  const schemeId = await page.evaluate(async () => {
    const modulePath = "/src/lib/theme.ts";
    const { saveScheme } = await import(/* @vite-ignore */ modulePath);
    return saveScheme({ name: "Agent surfaces", base: "light", tokens: {
      "--bg-app": "#f4f5f6", "--bg-panel": "#e1e2e3", "--bg-chip": "#c1c2c3",
      "--fg-body": "#314253", "--line": "#778899",
    } }).id as string;
  });
  await expect(page.locator("html")).toHaveAttribute("data-color-mode", "light");
  await expect(panel).toHaveCSS("background-color", "rgb(225, 226, 227)");
  await expect(prose).toHaveCSS("color", "rgb(49, 66, 83)");
  await expect(block).toHaveCSS("background-color", "rgb(244, 245, 246)");
  await expect(code.locator("code")).toHaveCSS("color", "rgb(49, 66, 83)");
  // This real SelectTrigger carries dark:bg-input/30. OS-based Tailwind dark
  // variants would override its chosen light bg-secondary with an input tint.
  await expect(model).toHaveCSS("background-color", "rgb(193, 194, 195)");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(model).toHaveCSS("background-color", "rgb(193, 194, 195)");

  await page.evaluate(async (id) => {
    const modulePath = "/src/lib/theme.ts";
    const { saveScheme } = await import(/* @vite-ignore */ modulePath);
    saveScheme({ id, name: "Agent surfaces", base: "light", tokens: {
      "--bg-app": "#101112", "--bg-panel": "#232425", "--bg-chip": "#414243",
      "--fg-body": "#d1d2d3", "--line": "#778899",
    } });
  }, schemeId);
  await expect(page.locator("html")).toHaveAttribute("data-color-mode", "dark");
  await expect(panel).toHaveCSS("background-color", "rgb(35, 36, 37)");
  await expect(prose).toHaveCSS("color", "rgb(209, 210, 211)");
  await expect(block).toHaveCSS("background-color", "rgb(16, 17, 18)");
  await expect(code.locator("code")).toHaveCSS("color", "rgb(209, 210, 211)");
  const darkTrigger = await model.evaluate((element) => getComputedStyle(element).backgroundColor);
  expect(darkTrigger).not.toBe("rgb(65, 66, 67)");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(model).toHaveCSS("background-color", darkTrigger);

  // Baker attaches the same React conversation surface to its permanent
  // identity. Navigating there must keep the saved custom palette intact.
  await page.goto("/baker");
  await expect(panel).toBeVisible({ timeout: 15_000 });
  await expect(panel).toHaveCSS("background-color", "rgb(35, 36, 37)");
  await expect(panel).toHaveCSS("color", "rgb(209, 210, 211)");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-color-mode", "dark");
  await expect(panel).toHaveCSS("background-color", "rgb(35, 36, 37)");
});
