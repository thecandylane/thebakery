// The shell's two tabs:
//   - "chat":  everything that is not the Baker — chats (full width), the
//              project studios, and the Floor;
//   - "baker": the Baker — its main conversation and its threads, apart
//              from Chat entirely.
// The mode is derived from the route (`modeForPath`), so the tabs and the
// sidebar can never disagree with where you are. Switching modes never
// changes permissions, agent identity, or runtime, and each mode remembers
// its own last location so switching back restores it.

import { modeForPath, type NavMode } from "./conversations";

export { modeForPath, type NavMode };

const LAST_KEY = "bakery:navMode:v4";
const HOME: Record<NavMode, string> = { chat: "/chat", baker: "/baker" };

function loadLast(): Record<NavMode, string> {
  const last = { ...HOME };
  try {
    const parsed = JSON.parse(localStorage.getItem(LAST_KEY) ?? "{}") as Partial<Record<NavMode, unknown>>;
    for (const mode of Object.keys(HOME) as NavMode[]) {
      const path = parsed[mode];
      if (typeof path === "string" && modeForPath(path.split("?")[0]) === mode) last[mode] = path;
    }
  } catch {
    // per-viewer convenience only; fall back to each mode's home
  }
  return last;
}

let last = $state<Record<NavMode, string>>(loadLast());
/** The last place outside Settings, in either mode. */
let place = $state("/");

export const navMode = {
  /** Remember the current location as its mode's last place. */
  remember(pathname: string, search = "") {
    // Settings is a window over the app, not a place to return to.
    if (pathname.startsWith("/settings")) return;
    const mode = modeForPath(pathname);
    // A one-off `?open=…` (the new-project form) is not a place to return to.
    const path = search.includes("open=") ? pathname : `${pathname}${search}`;
    place = path;
    if (!mode || last[mode] === path) return;
    last = { ...last, [mode]: path };
    try {
      localStorage.setItem(LAST_KEY, JSON.stringify(last));
    } catch {
      // per-viewer convenience only; ignore storage failures
    }
  },
  /** Drops a remembered place that turned out not to be one. A Chat link to
   *  a Baker conversation hands off to the Baker; remembered as Chat's last
   *  place, it sent the Chat tab straight back to the Baker every time. */
  forget(pathname: string) {
    const mode = modeForPath(pathname);
    if (!mode || last[mode].split("?")[0] !== pathname) return;
    last = { ...last, [mode]: HOME[mode] };
    try {
      localStorage.setItem(LAST_KEY, JSON.stringify(last));
    } catch {
      // per-viewer convenience only; ignore storage failures
    }
  },
  /** Where a mode's tab goes: its remembered location, or its home. */
  pathFor(mode: NavMode): string {
    return last[mode] ?? HOME[mode];
  },
  home(mode: NavMode): string {
    return HOME[mode];
  },
  /** Where closing Settings goes: the last place outside it. */
  returnPath(): string {
    return place;
  },
};
