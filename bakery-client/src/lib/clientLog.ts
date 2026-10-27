// A deployed app has no console anyone sees. Errors, and navigations that
// never finish, are reported to the node, which writes them to its log — so
// what broke in the app in use can be read and fixed without a screenshot.

import { node } from "./node";

const recent = new Map<string, number>();

/** Fire-and-forget; the same report at most once every 10 seconds. */
export function reportClient(kind: string, message: string, detail = ""): void {
  const key = `${kind}\u0000${message}`;
  const now = Date.now();
  if ((recent.get(key) ?? 0) > now - 10_000) return;
  recent.set(key, now);
  const path = typeof location === "undefined" ? "" : location.pathname + location.search;
  void node.clientLog({ kind, message, detail, path }).catch(() => {});
}

export function reportError(kind: string, error: unknown): void {
  if (error instanceof Error) reportClient(kind, `${error.name}: ${error.message}`, error.stack ?? "");
  else reportClient(kind, String(error));
}

let installed = false;

/** Reports every uncaught error and unhandled rejection in the app. */
export function installClientLog(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  window.addEventListener("error", (e) => reportError("error", e.error ?? e.message));
  window.addEventListener("unhandledrejection", (e) => {
    // Monaco rejects its cancelled work (a hover, a request for a closed
    // editor) with this by design; nothing broke.
    if (isMonacoCancellation(e.reason)) return;
    reportError("unhandled-rejection", e.reason);
  });
}

/** Monaco's `CancellationError`: name and message are both "Canceled". */
export function isMonacoCancellation(reason: unknown): boolean {
  return reason instanceof Error && reason.name === "Canceled" && reason.message === "Canceled";
}
