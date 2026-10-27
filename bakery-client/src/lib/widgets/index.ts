// Widget registry — single source of truth for ids, labels, icons, and notes.
//
// DockviewHost's "add panel" menu and the panel content factory both read
// this module. Never redefine WidgetId or list widget ids here — import
// from paneState.

import { WIDGET_IDS, type WidgetId } from "$lib/paneState";

export { WIDGET_IDS };

export type NoteCtx = Partial<{
  model:      string;   // active model name, e.g. "grok-4-fast"
  ctxK:       number;   // context used, in thousands of tokens (Task H only)
  pending:    number;   // proposals pending human review
  eventCount: number;
  live:       boolean;  // WS connection open
  /** Measured prompt_chars.total from GET /context — never tokens. */
  promptChars: number;
  worktree:   string;   // active worktree short name
  tabs:       number;   // open editor tabs
  dirty:      number;   // unsaved / dirty tabs
}>;

export type Widget = {
  id:    WidgetId;
  label: string;
  icon:  string;        // name passed to Icon.svelte
  note:  (ctx: NoteCtx) => string;
};

// Labels/notes from design_handoff WIDGETS.md. Blocked fields return "—" only
// when null: promptChars→GET /context, ctxK→H. Do not invent numbers.
export const WIDGETS = [
  {
    id:    "review",
    label: "Review queue",
    icon:  "ClipboardCheck",
    note:  (ctx: NoteCtx) =>
      ctx.pending != null ? `${ctx.pending} pending` : "—",
  },
  {
    id:    "events",
    label: "Event queue",
    icon:  "Activity",
    note:  (ctx: NoteCtx) =>
      ctx.eventCount != null
        ? `${ctx.eventCount.toLocaleString()} events${ctx.live ? " · live" : ""}`
        : "—",
  },
  {
    id:    "memory",
    label: "Memory & context",
    icon:  "Database",
    // Characters from GET /context — never "k tokens" or L1–L4 (no layers yet).
    note:  (ctx: NoteCtx) =>
      ctx.promptChars != null
        ? `${ctx.promptChars.toLocaleString()} chars`
        : "—",
  },
  {
    id:    "terminal",
    label: "Terminal",
    icon:  "Terminal",
    note:  (ctx: NoteCtx) => ctx.worktree ?? "—",
  },
  {
    id:    "code",
    label: "Code",
    icon:  "FileCode",
    note:  (ctx: NoteCtx) =>
      ctx.tabs != null
        ? `${ctx.tabs} tab${ctx.tabs === 1 ? "" : "s"} · ${ctx.dirty ?? 0} dirty`
        : "—",
  },
  {
    id:    "agents",
    label: "Agent map",
    icon:  "Users",
    note:  () => "every agent and its subagents",
  },
] as const satisfies readonly Widget[];

// Real exhaustiveness: cast describes what we HAVE so Record<WidgetId,true>
// can require every id. A missing entry → TS2741. (Cast-to-WidgetId was inert.)
type RegisteredId = (typeof WIDGETS)[number]["id"];
const _exhaustive: Record<WidgetId, true> = Object.fromEntries(
  WIDGETS.map((w) => [w.id, true]),
) as Record<RegisteredId, true>;
void _exhaustive;
