// Who may use the shell's bridge, the operator credential above all: the
// main window's own top frame, showing the app. It is src-tauri's rule (only
// the window labelled "main" may connect) plus the frame and the origin, so
// no view (a preview, code-server, devtools), no subframe and no page the
// main window was navigated to can ask. Plain node, so shell.check.ts tests
// it.

/** scheme://host[:port] of a URL. Node's URL gives a custom scheme like
 *  bakery:// the origin "null", so this builds it the way Chromium does. */
export function originOf(url: string): string | null {
  try {
    const u = new URL(url);
    return u.host ? `${u.protocol}//${u.host}` : null;
  } catch {
    return null;
  }
}

export type Sender = { contents: unknown; frame: unknown; url: string };
export type Trusted = { contents: unknown; frame: unknown; origin: string };

export function isTrustedSender(sender: Sender, trusted: Trusted): boolean {
  return (
    sender.contents === trusted.contents &&
    sender.frame != null &&
    sender.frame === trusted.frame &&
    originOf(sender.url) === trusted.origin
  );
}
