// A one-line text question in a small dialog: Electron has no window.prompt,
// so anything that asks the operator for a name (rename a thread or a
// project, a project's name) asks here. Plain DOM, so React and Svelte
// screens both use it. Colours are theme tokens only.

/** Resolves the trimmed answer, or null when the operator cancels. */
export function askText(label: string, initial = ""): Promise<string | null> {
  return new Promise((resolve) => {
    const before = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const scrim = document.createElement("div");
    scrim.setAttribute("role", "dialog");
    scrim.setAttribute("aria-modal", "true");
    scrim.setAttribute("aria-label", label);
    scrim.style.cssText =
      "position:fixed;inset:0;z-index:1000;display:grid;place-items:start center;padding-top:15vh;background:var(--overlay-scrim)";

    const form = document.createElement("form");
    form.style.cssText =
      "width:min(420px,90vw);display:flex;flex-direction:column;gap:8px;padding:12px;border:1px solid var(--line-strong);border-radius:8px;background:var(--bg-raised);box-shadow:var(--shadow-modal);color:var(--fg);font:12px var(--font-ui)";

    const title = document.createElement("label");
    title.textContent = label;
    title.style.cssText = "font-weight:700";
    const input = document.createElement("input");
    input.value = initial;
    input.setAttribute("aria-label", label);
    input.style.cssText =
      "padding:5px 8px;border:1px solid var(--line);border-radius:5px;background:var(--bg-app);color:var(--fg);font:inherit;outline:none";
    input.addEventListener("focus", () => (input.style.borderColor = "var(--accent)"));

    const buttons = document.createElement("div");
    buttons.style.cssText = "display:flex;justify-content:flex-end;gap:6px";
    const button = (text: string, primary: boolean) => {
      const b = document.createElement("button");
      b.type = primary ? "submit" : "button";
      b.textContent = text;
      b.style.cssText = primary
        ? "padding:4px 12px;border:1px solid var(--accent-fg);border-radius:5px;background:var(--grad-accent);color:var(--fg-on-solid);font:inherit;font-weight:700;cursor:pointer"
        : "padding:4px 12px;border:1px solid var(--line);border-radius:5px;background:var(--grad-button);color:var(--fg-2);font:inherit;cursor:pointer";
      return b;
    };
    const cancel = button("Cancel", false);
    buttons.append(cancel, button("OK", true));
    form.append(title, input, buttons);
    scrim.append(form);

    const done = (answer: string | null) => {
      scrim.remove();
      document.removeEventListener("keydown", onKey, true);
      before?.focus();
      resolve(answer);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      done(null);
    };
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      done(input.value.trim());
    });
    cancel.addEventListener("click", () => done(null));
    scrim.addEventListener("pointerdown", (e) => e.target === scrim && done(null));
    document.addEventListener("keydown", onKey, true);
    document.body.append(scrim);
    input.focus();
    input.select();
  });
}
