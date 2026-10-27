import { electronShell, isTauri } from "./desktop";

/** Ask the operator to confirm something.
 *
 *  On the Mac this is the Electron shell's own dialog (desktop.ts). Under
 *  Tauri it is `@tauri-apps/plugin-dialog`'s `confirm`, which goes over
 *  `plugin:dialog|message` — granted by `dialog:default`. Not
 *  `window.confirm` there: the plugin replaces it with an async shim that
 *  still sends the legacy `plugin:dialog|confirm`, which `dialog:default`
 *  does not grant, so the ACL refuses every confirmation. That shim also
 *  returns a Promise — always truthy — so a bare `if (!confirm(…)) return`
 *  goes ahead without an answer; call `ask` instead. In a browser tab (and
 *  the e2e suite) `window.confirm` is the browser's synchronous one. */
export async function ask(message: string): Promise<boolean> {
  const shell = electronShell();
  if (shell) return shell.confirm(message);
  if (isTauri()) {
    const { confirm } = await import("@tauri-apps/plugin-dialog");
    return confirm(message);
  }
  return window.confirm(message);
}
