// A workspace on screen, the operator's default view (the studio references,
// section 3): a top bar and the workspace's columns as groups of tabs — the
// first takes the room left, the others keep their width, like a side
// conversation. The niri strip stays one toggle away.

import { useEffect, useRef, useState, type DragEvent, type ReactNode } from "react";
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  Columns3Icon,
  LayoutGridIcon,
  LockIcon,
  PanelBottomIcon,
  PanelLeftIcon,
  PanelsTopLeftIcon,
  XIcon,
} from "lucide-react";
import * as L from "./layout";
import { PANEL_MIME, type SetLayout, StatusDot, startResize } from "./ui";

/** How a panel reads on its tab. `status` is set for conversations. */
export type PanelLabel = { title: string; icon: ReactNode; status: string | null };

/** Back and forward: framed, faded when there is nowhere to go. */
function NavBtn({ label, disabled, onClick, children }: { label: string; disabled: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      aria-label={label}
      className="grid size-[26px] shrink-0 place-items-center rounded-[5px] border border-(--line) bg-(image:--grad-button) text-(--fg-2) hover:text-(--fg) disabled:border-(--line-chrome) disabled:bg-none disabled:text-(--fg-ghost)"
      disabled={disabled}
      onClick={onClick}
      title={label}
      type="button"
    >
      {children}
    </button>
  );
}

/** One button of a framed group of toggles. */
function ToggleBtn({ label, pressed, first, onClick, children }: { label: string; pressed: boolean; first?: boolean; onClick: () => void; children: ReactNode }) {
  return (
    <button
      aria-label={label}
      aria-pressed={pressed}
      className={`grid h-6 w-7 place-items-center ${first ? "" : "border-(--line) border-l"} ${pressed ? "bg-(--bg-pressed) text-(--accent-fg)" : "bg-(image:--grad-button) text-(--fg-dim) hover:text-(--fg-2)"}`}
      onClick={onClick}
      title={label}
      type="button"
    >
      {children}
    </button>
  );
}

export function TopBar({
  title,
  sub,
  badge,
  canBack,
  canForward,
  onBack,
  onForward,
  onSearch,
  floor,
  onFloor,
  view,
  onView,
  sidebarOpen,
  onSidebar,
  bottomOpen,
  onBottom,
  waiting,
}: {
  title: string;
  sub: string;
  /** The workspace's tile: its colour and what it shows. */
  badge: { color: string; mark: ReactNode };
  canBack: boolean;
  canForward: boolean;
  onBack: () => void;
  onForward: () => void;
  /** The bar in the middle opens search and commands (⌘K). */
  onSearch: () => void;
  /** The floor: the zoomed-out view of the whole Bakery (⌘0). */
  floor: boolean;
  onFloor?: () => void;
  view: "screen" | "strip";
  onView?: () => void;
  sidebarOpen: boolean;
  onSidebar: () => void;
  bottomOpen: boolean;
  onBottom?: () => void;
  /** The pills left of the toggles: a restart's wait, this Mac, Hot reload
   *  and Deploy, and what waits for the operator (only while something does). */
  waiting?: ReactNode;
}) {
  return (
    <header className="flex h-10 shrink-0 items-center gap-2 border-(--line-chrome) border-b bg-(image:--grad-chrome) px-2.5">
      <div className="flex min-w-0 flex-1 gap-0.5">
        <NavBtn disabled={!canBack} label="Back" onClick={onBack}>
          <ChevronLeftIcon className="size-3.5" />
        </NavBtn>
        <NavBtn disabled={!canForward} label="Forward" onClick={onForward}>
          <ChevronRightIcon className="size-3.5" />
        </NavBtn>
      </div>
      <button
        aria-keyshortcuts="Meta+K"
        aria-label={`${title}${sub ? ` / ${sub}` : ""}: search and commands`}
        className="flex h-[26px] min-w-0 flex-[0_1_460px] items-center justify-center gap-2 rounded-[5px] border border-(--line) bg-(--bg-app) px-2.5 text-(--fg-dim) text-[11.5px] hover:border-(--line-strong)"
        onClick={onSearch}
        title="Search & commands (⌘K)"
        type="button"
      >
        <span
          className="grid size-4 shrink-0 place-items-center rounded font-bold text-(--fg-on-solid) text-[10px]"
          style={{ background: badge.color }}
        >
          {badge.mark}
        </span>
        <span className="shrink-0 font-bold text-(--fg)">{title}</span>
        {sub && (
          <>
            <span aria-hidden className="text-(--fg-ghost)">
              /
            </span>
            <span className="min-w-0 truncate">{sub}</span>
          </>
        )}
      </button>
      {/* No `min-w-0`: when the pills outgrow their share, the search bar
          gives way instead of the pills running under it. */}
      <div className="flex flex-1 items-center justify-end">
        {waiting}
        <span className="flex overflow-hidden rounded-md border border-(--line)">
          <ToggleBtn first label="Sidebar (⌘\)" onClick={onSidebar} pressed={sidebarOpen}>
            <PanelLeftIcon className="size-3.5" />
          </ToggleBtn>
          {/* The rest only with their handlers: with nothing open, there is
              no workspace for them to act on. */}
          {onBottom && (
            <ToggleBtn label="Terminal panel (⌃`)" onClick={onBottom} pressed={bottomOpen}>
              <PanelBottomIcon className="size-3.5" />
            </ToggleBtn>
          )}
          {onView && (
            <ToggleBtn label={view === "screen" ? "Show as columns" : "Show as tabs"} onClick={onView} pressed={false}>
              {view === "screen" ? <Columns3Icon className="size-3.5" /> : <PanelsTopLeftIcon className="size-3.5" />}
            </ToggleBtn>
          )}
          {onFloor && (
            <ToggleBtn label="Floor (⌘0)" onClick={onFloor} pressed={floor}>
              <LayoutGridIcon className="size-3.5" />
            </ToggleBtn>
          )}
        </span>
      </div>
    </header>
  );
}

type Drop = { col: string; where: "tab" | "before" | "after" };

/** The workspace's columns as groups of tabs. At the Baker's home page the
 *  first group shows the control plane in place of its tab's page, and none
 *  of its tabs is picked. */
export function Screen({
  workspace,
  setLayout,
  label,
  render,
  actions,
  place,
}: {
  workspace: L.Workspace;
  setLayout: SetLayout;
  label: (id: string) => PanelLabel;
  render: (id: string) => ReactNode;
  /** Buttons at the end of a group's tab bar, for the tab it shows. */
  actions: (id: string) => ReactNode;
  /** The workspace a dropped thread belongs in, when not this one. */
  place: (l: L.Layout, id: string) => string | null;
}) {
  const wsId = workspace.id;
  const groups = useRef(new Map<string, HTMLElement>());
  const [drop, setDrop] = useState<Drop | null>(null);
  useEffect(() => {
    const clear = () => setDrop(null);
    document.addEventListener("dragend", clear);
    return () => document.removeEventListener("dragend", clear);
  }, []);

  const carries = (e: DragEvent) => e.dataTransfer.types.includes(PANEL_MIME);
  function zone(e: DragEvent<HTMLElement>): Drop["where"] {
    const r = e.currentTarget.getBoundingClientRect();
    const x = (e.clientX - r.left) / r.width;
    return x < 0.25 ? "before" : x > 0.75 ? "after" : "tab";
  }
  /** Opens what was dropped (an agent from the sidebar, or a tab), then
   *  moves it: into the group as a tab after `target`, or into a new group
   *  on that side of it. */
  function onDrop(e: DragEvent<HTMLElement>, target: string | null, where: Drop["where"]) {
    const id = e.dataTransfer.getData(PANEL_MIME);
    setDrop(null);
    if (!id) return;
    e.preventDefault();
    setLayout((l) => {
      // A thread from another project opens in its own workspace instead.
      const home = place(l, id);
      if (home && home !== wsId) return L.openPanel(L.setActive(l, home), home, id);
      const opened = L.openPanel(l, wsId, id);
      if (!target) return opened;
      return L.movePanel(opened, wsId, id, target, where === "tab" ? "below" : where);
    });
  }

  const home = L.isHome(workspace);
  if (workspace.columns.length === 0) {
    return (
      <div
        aria-label={workspace.name}
        className="min-h-0 flex-1 bg-(--bg-app)"
        data-testid="shell-screen-empty"
        onDragOver={(e) => carries(e) && e.preventDefault()}
        onDrop={(e) => onDrop(e, null, "tab")}
        role="region"
      >
        {home && render(L.CONTROL)}
      </div>
    );
  }

  const focusCol = workspace.columns.findIndex((c) => c.panels.includes(workspace.focus ?? ""));
  return (
    <div className="flex min-h-0 flex-1 bg-(--bg-app)" data-testid="shell-screen">
      {workspace.columns.map((col, i) => {
        // At home the first group shows the control plane, which is no tab:
        // none is picked, and a click on its page focuses nothing.
        const tab = home && i === 0 ? L.CONTROL : L.tabOf(col, workspace.focus);
        const last = col.panels[col.panels.length - 1];
        const hint = drop?.col === col.id ? drop.where : null;
        return (
          <section
            aria-label={label(tab).title}
            className={`relative flex min-w-0 flex-col ${i > 0 ? "border-(--line-chrome) border-l" : ""}`}
            key={col.id}
            ref={(node) => {
              if (node) groups.current.set(col.id, node);
              else groups.current.delete(col.id);
            }}
            style={i === 0 ? { flex: "1 1 0" } : { flex: "none", width: col.width, maxWidth: "70%" }}
          >
            <div
              className={`flex h-[34px] shrink-0 items-end px-1.5 shadow-[inset_0_-1px_0_var(--line)] ${hint === "tab" ? "bg-(--accent-sel)" : "bg-(--bg-tabs)"}`}
              onDragLeave={(e) => {
                if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDrop(null);
              }}
              onDragOver={(e) => {
                if (!carries(e)) return;
                e.preventDefault();
                setDrop((d) => (d?.col === col.id && d.where === "tab" ? d : { col: col.id, where: "tab" }));
              }}
              onDrop={(e) => onDrop(e, last, "tab")}
            >
              <div aria-label="Tabs" className="flex min-w-0 flex-1 items-end overflow-x-auto [scrollbar-width:none]" role="tablist">
                {col.panels.map((id) => {
                  const lab = label(id);
                  const shown = id === tab;
                  return (
                    <div
                      className={`group/tab flex h-[29px] shrink-0 items-center ${shown ? "rounded-t-lg border border-(--line) border-b-0 bg-(--bg-panel)" : "border-(--line-row) border-r"}`}
                      key={id}
                      style={shown && i === focusCol ? { boxShadow: "inset 0 2px 0 var(--accent)" } : undefined}
                    >
                      <button
                        aria-selected={shown}
                        className={`flex h-full max-w-56 items-center gap-2 pr-1 pl-3 text-[11.5px] ${shown ? "font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
                        data-testid="shell-tab"
                        draggable
                        onClick={() => setLayout((l) => L.focusPanel(l, wsId, id))}
                        onDragStart={(e) => {
                          e.dataTransfer.setData(PANEL_MIME, id);
                          e.dataTransfer.effectAllowed = "move";
                        }}
                        role="tab"
                        title={lab.title}
                        type="button"
                      >
                        {lab.status ? <StatusDot size={12} status={lab.status} streamId={id} /> : lab.icon}
                        <span className="truncate">{lab.title}</span>
                      </button>
                      {L.isLocked(workspace, id) ? (
                        <span className="mr-1.5 grid size-5 place-items-center text-(--fg-faint)" title="Locked">
                          <LockIcon aria-label="Locked" className="size-3" />
                        </span>
                      ) : (
                        <button
                          aria-label={`Close ${lab.title}`}
                          className={`mr-1.5 grid size-5 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2) focus:opacity-100 ${shown ? "" : "opacity-0 group-hover/tab:opacity-100"}`}
                          onClick={() => setLayout((l) => L.closePanel(l, wsId, id))}
                          title="Close"
                          type="button"
                        >
                          <XIcon className="size-3" />
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
              <span className="mb-1 flex shrink-0 items-center self-end pl-1.5">{actions(tab)}</span>
            </div>
            <div
              aria-label={label(tab).title}
              className="relative min-h-0 flex-1 bg-(--bg-panel)"
              data-shell-clip
              onDragLeave={(e) => {
                if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDrop(null);
              }}
              onDragOver={(e) => {
                if (!carries(e)) return;
                e.preventDefault();
                const where = zone(e);
                setDrop((d) => (d?.col === col.id && d.where === where ? d : { col: col.id, where }));
              }}
              onDrop={(e) => onDrop(e, tab, zone(e))}
              onPointerDownCapture={() => setLayout((l) => L.focusPanel(l, wsId, tab))}
              role="tabpanel"
              style={{
                boxShadow:
                  hint === "before" ? "inset 3px 0 0 var(--accent)" : hint === "after" ? "inset -3px 0 0 var(--accent)" : undefined,
              }}
            >
              <div className="h-full" key={tab}>
                {render(tab)}
              </div>
            </div>
            {i > 0 && (
              <div
                aria-hidden
                className="absolute inset-y-0 -left-1 z-10 w-2 cursor-col-resize"
                onPointerDown={(e) =>
                  startResize(e, groups.current.get(col.id) ?? null, -1, [L.COLUMN_MIN, L.COLUMN_MAX], (width) =>
                    setLayout((l) => L.resizeColumn(l, wsId, col.id, width)),
                  )
                }
              />
            )}
          </section>
        );
      })}
    </div>
  );
}
