// Whether the window shows (the page's visibility: not minimised, on this
// Space, not covered). A poll that only keeps a picture current skips its
// turns while it doesn't — each one asks the node for work (a studio picture
// runs git per task worktree) that nobody sees — and asks once the moment it
// shows again, so what shows is as current as it was before the hide.

export function pageHidden(): boolean {
  return typeof document !== "undefined" && document.hidden;
}

/** `cb` each time the window shows again; returns its undo. */
export function onShown(cb: () => void): () => void {
  if (typeof document === "undefined") return () => {};
  const changed = () => {
    if (!document.hidden) cb();
  };
  document.addEventListener("visibilitychange", changed);
  return () => document.removeEventListener("visibilitychange", changed);
}

/** `fn` every `ms` while the window shows; a turn skipped while it was
 *  hidden is taken the moment it shows. Returns its stop. */
export function whileShown(fn: () => void, ms: number): () => void {
  let missed = false;
  const timer = setInterval(() => {
    if (pageHidden()) missed = true;
    else fn();
  }, ms);
  const stop = onShown(() => {
    if (!missed) return;
    missed = false;
    fn();
  });
  return () => {
    clearInterval(timer);
    stop();
  };
}
