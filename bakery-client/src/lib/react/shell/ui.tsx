// Small pieces the shell and its tool panels share.

import { createContext, useContext, useEffect, type PointerEvent as ReactPointerEvent, type ReactNode } from "react";
import { RotateCwIcon } from "lucide-react";
import { electronShell } from "$lib/desktop";
import { whileShown } from "$lib/pageShown";
import { updatedAgo, useMinute } from "./cache";
import type * as L from "./layout";
import { useLivePhase } from "./livePhases";
import type { ShellSnapshot, ShellStore } from "./shellSource.svelte";
import { type Glyph, glyphOf, type Merge } from "./threadStates";

export type SetLayout = (f: (l: L.Layout) => L.Layout) => void;

/** What a tool panel gets from the shell it sits in. */
export type ToolContext = {
  store: ShellStore;
  snap: ShellSnapshot;
  /** The whole window's: every workspace, the Baker's side. */
  layout: L.Layout;
  workspace: L.Workspace;
  setLayout: SetLayout;
  /** The workspace's colour, as its mark shows it in the switcher. */
  color: string;
};

/** What a dragged panel or agent row carries: its stream id. */
export const PANEL_MIME = "application/x-bakery-panel";

/** The node's own words for a failed call (its 409 and 504 bodies say
 *  exactly what to do). */
export const said = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** A cached panel's freshness: "updated N min ago" and Reload; a failed
 *  refresh says so and keeps the data it had. */
export function Freshness({ at, busy, error, onReload }: { at: number | null; busy: boolean; error?: string; onReload: () => void }) {
  useMinute();
  return (
    <span className="flex shrink-0 items-center gap-1 text-(--fg-faint) text-[10.5px]" data-testid="freshness">
      {error ? (
        <span className="max-w-48 truncate text-(--bad)" title={error}>
          didn't refresh
        </span>
      ) : (
        at !== null && <span title={new Date(at).toLocaleString()}>{updatedAgo(at)}</span>
      )}
      <button
        aria-label="Reload"
        className="grid size-6 place-items-center rounded-[5px] text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg) disabled:opacity-60"
        disabled={busy}
        onClick={onReload}
        title="Reload"
        type="button"
      >
        <RotateCwIcon className={`size-3.5 ${busy ? "animate-spin" : ""}`} />
      </button>
    </span>
  );
}

/** Workspace colours, by position — tokens only, never literal colours. */
const WS_COLORS = ["--accent", "--ok", "--violet", "--warn", "--syntax-type", "--bad", "--syntax-string", "--accent-fg"];
export const workspaceColor = (i: number) => `var(${WS_COLORS[i % WS_COLORS.length]})`;

/** What the shell knows beyond a thread's status: whether its finished
 *  branch has something to merge. (Its live phase is every glyph's own, from
 *  `livePhases`.) */
export type ThreadStates = { merge(streamId: string): Merge | null };
export const ThreadStatesContext = createContext<ThreadStates>({ merge: () => null });

const GLYPH_COLOR: Record<Glyph, string> = {
  needs: "var(--warn)",
  failed: "var(--bad)",
  working: "var(--ok)",
  writing: "var(--link)",
  thinking: "var(--violet)",
  conflict: "var(--warn)",
  ready: "var(--ok)",
  done: "var(--ok)",
  stopped: "var(--idle)",
  idle: "var(--idle)",
};

/** Each state's bakery glyph (Round 5, M2): its shape, and the parts that move. */
function glyphArt(g: Glyph): { wide: boolean; art: ReactNode } {
  switch (g) {
    case "needs":
      return { wide: false, art: <path d="M3 11h10M4.5 11a3.5 3.5 0 0 1 7 0M8 6V4.5M7 4.5h2" /> };
    case "working":
      return { wide: true, art: <><path d="M1.5 8h2M14.5 8h2" /><rect height="5" rx="2.5" width="10" x="4" y="5.5" /></> };
    case "thinking":
      return { wide: false, art: <><path d="M2.5 12.5h11" /><path className="bk-puff" d="M3.5 12.5c0-3 2-5.5 4.5-5.5s4.5 2.5 4.5 5.5" /></> };
    case "writing":
      return { wide: false, art: <><path d="M10 2.5 13.5 6 8 11.5H4.5V8Z" /><path className="bk-ink" d="M2 14c2-1.5 3.5-1.5 5.5 0" /></> };
    case "ready":
      return { wide: true, art: <><circle className="bk-l" cx="6" cy="8" r="3.2" /><circle className="bk-r" cx="12" cy="8" r="3.2" /></> };
    case "conflict":
      return { wide: true, art: <><circle cx="5" cy="8" r="3" /><circle cx="13" cy="8" r="3" /><path d="M9 5.5v5" strokeDasharray="1.5 1.5" /></> };
    case "done":
      return { wide: true, art: <><path d="M2.5 12V9a6.5 4.5 0 0 1 13 0v3Z" /><path className="bk-steam" d="M7 4c0-1 1-1 1-2M10.5 4c0-1 1-1 1-2" /></> };
    case "failed":
      return { wide: true, art: <><path d="M2.5 12V9a6.5 4.5 0 0 1 13 0v3Z" /><path d="M7 7.5l4 3M11 7.5l-4 3" /></> };
    case "stopped":
      return { wide: false, art: <rect fill="currentColor" height="6" rx="1.2" stroke="none" width="6" x="5" y="5" /> };
    default:
      return { wide: false, art: <circle cx="8" cy="8" fill="currentColor" r="3" stroke="none" /> };
  }
}

/** A thread's state as its bakery glyph: dough rising while it thinks, a
 *  rolling pin while it works, a loaf when it's done, two doughs pressing
 *  into one when its branch is ready to merge, a bell when it needs you.
 *  The same glyph on every surface; only its size changes. */
export function StatusDot({ status, title, streamId, size = 14 }: { status: string; title?: string; streamId?: string; size?: number }) {
  const states = useContext(ThreadStatesContext);
  const phase = useLivePhase(streamId);
  const g = glyphOf(status, phase, streamId ? states.merge(streamId) : null);
  const { wide, art } = glyphArt(g);
  return (
    <svg
      aria-hidden={title ? undefined : true}
      className={`bk-glyph bk-${g}`}
      data-glyph={g}
      fill="none"
      height={size}
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth={1.7}
      style={{ color: GLYPH_COLOR[g] }}
      viewBox={wide ? "0 0 18 16" : "0 0 16 16"}
      width={wide ? (size * 18) / 16 : size}
    >
      {title && <title>{title}</title>}
      {art}
    </svg>
  );
}

export type ChipTone = "neutral" | "ok" | "warn" | "bad" | "teal" | "accent";
const CHIP: Record<ChipTone, string> = {
  neutral: "bg-(--chip-bg) text-(--chip-fg)",
  ok: "bg-(--chip-ok-bg) font-bold text-(--chip-ok-fg)",
  warn: "bg-(--chip-warn-bg) font-bold text-(--chip-warn-fg)",
  bad: "bg-(--chip-bad-bg) font-bold text-(--chip-bad-fg)",
  teal: "bg-(--chip-teal-bg) font-bold text-(--chip-teal-fg)",
  accent: "bg-(image:--grad-accent) font-bold text-(--fg-on-solid)",
};

/** A small filled label for a count or a state. */
export function Chip({ tone = "neutral", title, children }: { tone?: ChipTone; title?: string; children: ReactNode }) {
  return (
    <span className={`shrink-0 whitespace-nowrap rounded px-[5px] text-[10px] leading-4 ${CHIP[tone]}`} title={title}>
      {children}
    </span>
  );
}

/** How many views show a node reply marked `secret` right now. */
let secretsShown = 0;

/** While `on` (the view shows a node reply marked `secret`), the desktop
 *  shell blocks screenshots; it allows them again once no view shows one. */
export function useSecretShown(on: boolean) {
  useEffect(() => {
    if (!on) return;
    if (secretsShown++ === 0) void electronShell()?.secretShown(true);
    return () => {
      if (--secretsShown === 0) void electronShell()?.secretShown(false);
    };
  }, [on]);
}

/** Runs `fn` now and every `ms` while the window is visible, and once as it
 *  shows again after a skipped turn (`whileShown`). */
export function usePoll(fn: () => void, ms: number | null) {
  useEffect(() => {
    if (ms === null) return;
    fn();
    return whileShown(fn, ms);
  }, [fn, ms]);
}

/** Drags a width (or with `axis` "y" a height) live on the element itself
 *  (no React render per pixel) and commits once on release. `sign` is -1
 *  for a handle on the left (or top) edge. */
export function startResize(
  e: ReactPointerEvent,
  el: HTMLElement | null,
  sign: 1 | -1,
  [min, max]: [number, number],
  commit: (size: number) => void,
  axis: "x" | "y" = "x",
) {
  if (!el) return;
  e.preventDefault();
  const handle = e.currentTarget as HTMLElement;
  const at = (ev: { clientX: number; clientY: number }) => (axis === "x" ? ev.clientX : ev.clientY);
  const start = at(e);
  const rect = el.getBoundingClientRect();
  const startSize = axis === "x" ? rect.width : rect.height;
  const size = (p: number) => Math.round(Math.min(max, Math.max(min, startSize + sign * (p - start))));
  handle.setPointerCapture(e.pointerId);
  const move = (ev: PointerEvent) => {
    el.style[axis === "x" ? "width" : "height"] = `${size(at(ev))}px`;
  };
  const up = (ev: PointerEvent) => {
    handle.removeEventListener("pointermove", move);
    handle.removeEventListener("pointerup", up);
    commit(size(at(ev)));
  };
  handle.addEventListener("pointermove", move);
  handle.addEventListener("pointerup", up);
}
