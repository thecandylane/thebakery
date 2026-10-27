// Files dropped or pasted anywhere in the app attach to the chat in use: the
// composer the operator last clicked or typed in, else the newest one shown.
// One set of document listeners serves every composer, so a drop never lands
// in two chats, and a file dropped outside any composer is never opened by
// the browser in place of the app.

export type AttachTarget = {
  /** Adds files under the composer's own size and count checks; `false`
   *  when this chat has no composer right now (a pane with no model yet). */
  add: (files: File[]) => boolean;
  /** Shows or hides the composer's "Drop to attach" cue. */
  setDropping: (dropping: boolean) => void;
};

const targets = new Map<symbol, AttachTarget>();
let active: symbol | null = null;
let shown: AttachTarget | null = null;
let installed = false;

function current(): AttachTarget | null {
  if (active && targets.has(active)) return targets.get(active) ?? null;
  return [...targets.values()].at(-1) ?? null;
}

const hasFiles = (data: DataTransfer | null) => !!data && Array.from(data.types).includes("Files");

/** The chat in use takes the files; when it cannot, the newest one that can
 *  does — a dropped or pasted file never just vanishes. */
function deliver(files: File[]) {
  const first = current();
  if (first?.add(files)) return;
  for (const target of [...targets.values()].reverse()) {
    if (target !== first && target.add(files)) return;
  }
}

function cue(target: AttachTarget | null) {
  if (shown === target) return;
  shown?.setDropping(false);
  shown = target;
  shown?.setDropping(true);
}

/** A field the operator is typing in keeps any paste that carries text. */
function typingField(el: EventTarget | null): boolean {
  return el instanceof HTMLElement && (el.isContentEditable || el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement);
}

function install() {
  if (installed || typeof document === "undefined") return;
  installed = true;
  document.addEventListener("dragover", (e) => {
    if (!hasFiles(e.dataTransfer)) return;
    e.preventDefault();
    cue(current());
  });
  document.addEventListener("dragleave", (e) => {
    // Leaving the window has no related target.
    if (e.relatedTarget === null) cue(null);
  });
  document.addEventListener("drop", (e) => {
    cue(null);
    if (!hasFiles(e.dataTransfer)) return;
    // A drop on a composer's own box was already taken there.
    if (e.defaultPrevented) return;
    e.preventDefault();
    const files = Array.from(e.dataTransfer?.files ?? []);
    if (files.length > 0) deliver(files);
  });
  document.addEventListener("dragend", () => cue(null));
  document.addEventListener("paste", (e) => {
    if (e.defaultPrevented) return;
    if (typingField(e.target) && e.clipboardData?.types.includes("text/plain")) return;
    const files = Array.from(e.clipboardData?.files ?? []);
    if (files.length === 0) return;
    e.preventDefault();
    deliver(files);
  });
}

/** Registers a composer. Call `activate` when the operator clicks or types in
 *  its chat; `dispose` when it unmounts. */
export function registerAttachTarget(target: AttachTarget): { activate: () => void; dispose: () => void } {
  install();
  const id = Symbol("attach-target");
  targets.set(id, target);
  return {
    activate: () => {
      active = id;
    },
    dispose: () => {
      targets.delete(id);
      if (active === id) active = null;
      if (shown === target) shown = null;
    },
  };
}
