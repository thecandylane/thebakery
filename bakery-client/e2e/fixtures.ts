// The browser harness's credential seam.
//
// Bakery is a desktop app: in the real product the operator credential
// reaches the window natively (Tauri reads ~/.bakery/node.json, proves it
// against the node, and returns it over IPC — `src-tauri/src/connect.rs`).
// A Playwright page is not the desktop shell, so it has no native
// bootstrap; these tests inject a token at RUNTIME instead.
//
// Runtime injection, deliberately — not `VITE_BAKERY_OPERATOR_TOKEN` as
// before: a VITE_-prefixed variable is compiled into the frontend bundle,
// which is exactly where a credential must never live. Nothing here ships:
// `addInitScript` runs only in pages this harness creates.
//
// Every spec must import `test` from here rather than from
// `@playwright/test`, or its pages will have no credential and the app
// will (correctly) show the connection screen instead of the studio.
import { test as base } from "@playwright/test";

export { expect } from "@playwright/test";

/** Set by `playwright.config.ts` before any worker imports this module —
 *  the same value it hands the node as `BAKERY_OPERATOR_TOKEN`. */
export const OPERATOR_TOKEN = process.env.BAKERY_E2E_OPERATOR_TOKEN;
if (!OPERATOR_TOKEN) {
  throw new Error(
    "BAKERY_E2E_OPERATOR_TOKEN is not set — e2e/fixtures.ts must only be imported from a Playwright run " +
      "driven by playwright.config.ts, which always sets it before any worker starts.",
  );
}

// Injected on the CONTEXT, not the page: several specs open a second
// Studio page with `context.newPage()` to prove live cross-page updates,
// and a per-page script would leave those pages credential-less (they
// would correctly show the connection screen and the spec would fail on a
// missing pane). Context-level covers every page a test opens.
export const test = base.extend({
  context: async ({ context }, use) => {
    await context.addInitScript((token) => {
      (window as unknown as { __BAKERY_OPERATOR_TOKEN__?: string }).__BAKERY_OPERATOR_TOKEN__ = token;
    }, OPERATOR_TOKEN);
    await use(context);
  },
});
