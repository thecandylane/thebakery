// The factory floor: every workspace zoomed out into a row of live cards, in
// the layout's own order, then the workspace's agents that have no panel
// yet. Each seat agent's card carries the tree of subagents it launched (the
// Agent map, scoped to that agent). Click a card to dive back in.

import type { ReactNode } from "react";
import { statusLabel, type Conversation } from "$lib/conversations";
import type { AgentMapNode } from "$lib/node";
import { floorCards, membersOf, subagentTree } from "./floorModel";
import * as L from "./layout";
import type { ShellSnapshot } from "./shellSource.svelte";
import { type SetLayout, StatusDot, workspaceColor } from "./ui";


export function Floor({
  snap,
  layout,
  nodes,
  toolLabel,
  setLayout,
  onClose,
  home,
}: {
  snap: ShellSnapshot;
  layout: L.Layout;
  nodes: readonly AgentMapNode[];
  /** A tool panel's title and icon by its layout id, or null for a conversation. */
  toolLabel: (id: string) => { title: string; icon: ReactNode } | null;
  setLayout: SetLayout;
  onClose: () => void;
  /** The Bakery's page with nothing open: there is nothing to go back to. */
  home: boolean;
}) {
  const byId = new Map(snap.conversations.map((c) => [c.streamId, c]));
  const baker = snap.conversations.filter((c) => c.bakerThread);

  function dive(wsId: string, id: string) {
    setLayout((l) => L.openPanel(l, wsId, id));
    onClose();
  }

  return (
    <div className="min-h-0 flex-1 space-y-4 overflow-auto p-3" data-testid="shell-floor">
      {!home && <p className="text-(--fg-ghost) text-[11px]">Factory floor · click a card to dive in · ⌘0, Esc or pinch to go back</p>}

      {baker.length > 0 && (
        <section>
          <h2 className="mb-1.5 flex items-center gap-2 text-(--fg-2) text-xs">Baker</h2>
          <div className="flex flex-wrap gap-2">
            {baker.map((c) => (
              <ConvCard
                conv={c}
                focused={layout.mode === "baker" && layout.baker.focus === c.streamId}
                key={c.streamId}
                onClick={() => dive(L.BAKER, c.streamId)}
                open
                subagents={subagentTree(nodes, c.streamId)}
              />
            ))}
          </div>
        </section>
      )}

      {[...layout.workspaces, ...layout.orcs].map((w, at) => {
        // Coloured by its place in its own group (the Baker's, or its
        // project's), as the sidebar colours it.
        const i = w.orc ? layout.orcs.filter((o) => (o.projectId ?? null) === (w.projectId ?? null)).indexOf(w) : at;
        const mine = membersOf(w, snap.projects, snap.conversations);
        const cards = floorCards(w, mine.map((c) => c.streamId));
        const needs = mine.filter((c) => c.status === "needs_input").length;
        const working = mine.filter((c) => c.status === "working").length;
        return (
          <section key={w.id}>
            <h2 className="mb-1.5 flex items-center gap-2 text-xs">
              <span className="size-2 rounded-full" style={{ background: workspaceColor(i) }} />
              <span className={w.id === layout.active ? "text-(--fg)" : "text-(--fg-2)"}>{w.name}</span>
              {needs > 0 && <span className="text-(--warn) text-[10px]">{needs} need you</span>}
              {working > 0 && <span className="text-(--fg-faint) text-[10px]">{working} working</span>}
              {!w.orc && i < 9 && <span className="text-(--fg-ghost) text-[10px]">⌃{i + 1}</span>}
            </h2>
            <div className="flex flex-wrap gap-2">
              {cards.length === 0 && <p className="text-(--fg-faint) text-[11px]">Nothing here yet.</p>}
              {cards.map((card) => {
                const focused = w.id === layout.active && w.focus === card.id;
                const tool = toolLabel(card.id);
                if (tool) {
                  return (
                    <button
                      className={`flex w-56 items-center gap-1.5 rounded-xl border bg-(--bg-panel) p-2 text-left text-(--fg-2) text-xs hover:border-(--accent-border) ${focused ? "border-(--accent-border)" : "border-(--line-soft)"}`}
                      key={card.id}
                      onClick={() => dive(w.id, card.id)}
                      type="button"
                    >
                      {tool.icon} {tool.title}
                    </button>
                  );
                }
                const conv = byId.get(card.id);
                if (!conv) return null;
                return (
                  <ConvCard
                    conv={conv}
                    focused={focused}
                    key={card.id}
                    onClick={() => dive(w.id, card.id)}
                    open={card.open}
                    subagents={subagentTree(nodes, card.id)}
                  />
                );
              })}
            </div>
          </section>
        );
      })}
    </div>
  );
}

function ConvCard({
  conv,
  subagents,
  open,
  focused,
  onClick,
}: {
  conv: Conversation;
  subagents: { node: AgentMapNode; depth: number }[];
  open: boolean;
  focused: boolean;
  onClick: () => void;
}) {
  // Every one: the list scrolls rather than folding into "+N more".
  const shown = subagents;
  return (
    <button
      className={`block w-56 rounded-xl border p-2 text-left hover:border-(--accent-border) ${focused ? "border-(--accent-border) bg-(--bg-panel)" : open ? "border-(--line-soft) bg-(--bg-panel)" : "border-(--line-soft) border-dashed bg-transparent"}`}
      data-testid="shell-floor-card"
      onClick={onClick}
      title={open ? conv.title : `${conv.title} (no panel yet: click to open one)`}
      type="button"
    >
      <span className="flex items-center gap-1.5 text-(--fg) text-xs">
        <StatusDot size={16} status={conv.status} streamId={conv.streamId} />
        <span className="truncate">{conv.title}</span>
      </span>
      <span className="mt-0.5 block truncate text-(--fg-dim) text-[11px]">
        {statusLabel(conv.status)}
        {conv.note ? ` · ${conv.note}` : ""}
      </span>
      {shown.length > 0 && (
        <span className="mt-1 block max-h-40 space-y-0.5 overflow-auto border-(--line-soft) border-t pt-1" onClick={(e) => e.target === e.currentTarget && e.stopPropagation()}>
          {shown.map(({ node, depth }) => (
            <span
              className="flex items-center gap-1.5 text-(--fg-dim) text-[11px]"
              key={node.id}
              style={{ paddingLeft: depth * 10 }}
              title={node.note ?? node.label}
            >
              <StatusDot status={node.status} streamId={node.kind === "subagent" ? undefined : node.stream_id} />
              <span className="truncate">{node.label}</span>
            </span>
          ))}
        </span>
      )}
    </button>
  );
}
