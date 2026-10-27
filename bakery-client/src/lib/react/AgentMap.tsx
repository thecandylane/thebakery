// The Agent map: every agent and who started whom — the Baker and its
// threads, the tasks and chats a Baker conversation created, project agents,
// and every subagent a native runtime launched in its latest turn — as a
// tree you can pan and zoom. Click a node to open its conversation (for a
// subagent: its parent's, with the subagent's card open).

import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent, type WheelEvent } from "react";
import {
  BotIcon,
  ChefHatIcon,
  FolderGit2Icon,
  ListTodoIcon,
  MessageCircleIcon,
  MessagesSquareIcon,
  MinusIcon,
  PlusIcon,
  ScanIcon,
} from "lucide-react";
import { node as api, type AgentMapNode } from "$lib/node";
import { onEvent } from "$lib/events";
import { whileShown } from "$lib/pageShown";
import { CARD_H, CARD_W, layout, live, visibleNodes } from "./agentMapLayout";

export type AgentMapProps = {
  /** Open this node's conversation. */
  open(node: AgentMapNode): void;
};

const KIND_ICON = {
  baker: ChefHatIcon,
  thread: MessagesSquareIcon,
  task: ListTodoIcon,
  chat: MessageCircleIcon,
  project: FolderGit2Icon,
  subagent: BotIcon,
} as const;

const STATUS_WORD: Record<AgentMapNode["status"], string> = {
  working: "working",
  needs_input: "needs you",
  failed: "failed",
  done: "done",
  idle: "idle",
  stopped: "stopped",
};

function nodeLine(n: AgentMapNode): string {
  const parts = [STATUS_WORD[n.status]];
  if (n.kind === "subagent") parts.push(`${n.steps} ${n.steps === 1 ? "step" : "steps"}`);
  if (n.detail) parts.push(n.detail);
  return parts.join(" · ");
}

/** Every agent and who started whom, kept live. Shared by the map and the
 *  shell's agent cards, so both show the same tree. */
export function useAgentMapNodes(): { nodes: AgentMapNode[] | null; error: string } {
  const [nodes, setNodes] = useState<AgentMapNode[] | null>(null);
  const [error, setError] = useState("");
  /** The last answer as the node said it. */
  const said = useRef("");

  const load = useCallback(async () => {
    try {
      const map = await api.agentMap();
      // A reload that says nothing new keeps the same list, so nothing that
      // shows it (the whole shell) draws again.
      const now = JSON.stringify(map.nodes);
      if (now !== said.current) {
        said.current = now;
        setNodes(map.nodes);
      }
      setError("");
    } catch (e) {
      setError(`${e}`);
    }
  }, []);

  // Live: any fact on any stream may move a node (a turn, a tool call, a new
  // conversation); bursts settle into one reload. While something works, a
  // slow poll keeps its steps current between facts.
  useEffect(() => {
    void load();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const stop = onEvent(null, () => {
      clearTimeout(timer);
      timer = setTimeout(() => void load(), 400);
    });
    return () => {
      clearTimeout(timer);
      stop();
    };
  }, [load]);
  const working = nodes?.some(live) ?? false;
  useEffect(() => {
    if (!working) return;
    return whileShown(() => void load(), 3000);
  }, [load, working]);

  return { nodes, error };
}

export function AgentMap({ open }: AgentMapProps) {
  const { nodes, error } = useAgentMapNodes();
  const [all, setAll] = useState(false);
  const [view, setView] = useState({ x: 16, y: 16, k: 1 });
  const viewport = useRef<HTMLDivElement>(null);
  const fitted = useRef(false);
  const drag = useRef<{ x: number; y: number } | null>(null);

  const shown = useMemo(() => visibleNodes(nodes ?? [], all), [nodes, all]);
  const { placed, width, height } = useMemo(() => layout(shown), [shown]);
  const at = useMemo(() => new Map(placed.map((p) => [p.node.id, p])), [placed]);

  const fit = useCallback(() => {
    const box = viewport.current?.getBoundingClientRect();
    if (!box || box.width === 0) return;
    const k = Math.min(1, (box.width - 32) / width, (box.height - 32) / height);
    setView({ k, x: (box.width - width * k) / 2, y: 16 });
  }, [width, height]);
  useEffect(() => {
    if (fitted.current || placed.length === 0) return;
    fitted.current = true;
    fit();
  }, [fit, placed.length]);

  const zoomBy = useCallback((factor: number, cx?: number, cy?: number) => {
    const box = viewport.current?.getBoundingClientRect();
    setView((v) => {
      const k = Math.min(2, Math.max(0.25, v.k * factor));
      const px = cx ?? (box ? box.width / 2 : 0);
      const py = cy ?? (box ? box.height / 2 : 0);
      // Keep the point under the pointer where it is.
      return { k, x: px - ((px - v.x) * k) / v.k, y: py - ((py - v.y) * k) / v.k };
    });
  }, []);
  const onWheel = (e: WheelEvent<HTMLDivElement>) => {
    const box = viewport.current?.getBoundingClientRect();
    zoomBy(Math.exp(-e.deltaY * 0.0015), box ? e.clientX - box.left : undefined, box ? e.clientY - box.top : undefined);
  };
  const onPointerDown = (e: PointerEvent<HTMLDivElement>) => {
    if ((e.target as HTMLElement).closest("[data-agent-node]")) return;
    drag.current = { x: e.clientX, y: e.clientY };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: PointerEvent<HTMLDivElement>) => {
    const from = drag.current;
    if (!from) return;
    drag.current = { x: e.clientX, y: e.clientY };
    setView((v) => ({ ...v, x: v.x + e.clientX - from.x, y: v.y + e.clientY - from.y }));
  };
  const onPointerUp = () => {
    drag.current = null;
  };

  const counts = useMemo(() => {
    const list = nodes ?? [];
    return {
      agents: list.filter((n) => n.kind !== "subagent").length,
      subagents: list.filter((n) => n.kind === "subagent").length,
      working: list.filter((n) => n.status === "working").length,
      waiting: list.filter((n) => n.status === "needs_input").length,
    };
  }, [nodes]);

  return (
    <div className="bakery-agent-map" data-testid="agent-map">
      <div className="bakery-agent-map-bar">
        <span data-testid="agent-map-counts">
          {counts.agents} {counts.agents === 1 ? "agent" : "agents"} · {counts.subagents}{" "}
          {counts.subagents === 1 ? "subagent" : "subagents"}
          {counts.working > 0 && ` · ${counts.working} working`}
          {counts.waiting > 0 && ` · ${counts.waiting} need you`}
        </span>
        <label className="bakery-agent-map-toggle">
          <input checked={all} onChange={(e) => setAll(e.target.checked)} type="checkbox" />
          Show all
        </label>
        <button aria-label="Zoom out" onClick={() => zoomBy(1 / 1.2)} title="Zoom out" type="button">
          <MinusIcon className="size-3.5" />
        </button>
        <button aria-label="Fit" onClick={fit} title="Fit everything in view" type="button">
          <ScanIcon className="size-3.5" />
        </button>
        <button aria-label="Zoom in" onClick={() => zoomBy(1.2)} title="Zoom in" type="button">
          <PlusIcon className="size-3.5" />
        </button>
      </div>
      {error && <div className="bakery-agent-map-empty">The map could not load: {error}</div>}
      {nodes && shown.length === 0 && !error && <div className="bakery-agent-map-empty">No agents yet.</div>}
      <div
        className="bakery-agent-map-view"
        onPointerCancel={onPointerUp}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onWheel={onWheel}
        ref={viewport}
      >
        <div
          className="bakery-agent-map-world"
          style={{ width, height, transform: `translate(${view.x}px, ${view.y}px) scale(${view.k})` }}
        >
          <svg className="bakery-agent-map-edges" height={height} width={width}>
            {placed.map(({ node: n, x, y }) => {
              const parent = n.parent ? at.get(n.parent) : undefined;
              if (!parent) return null;
              const top = parent.y + CARD_H;
              const mid = (top + y) / 2;
              return (
                <path
                  d={`M ${parent.x} ${top} C ${parent.x} ${mid}, ${x} ${mid}, ${x} ${y}`}
                  data-status={n.status}
                  data-testid="agent-edge"
                  key={n.id}
                />
              );
            })}
          </svg>
          {placed.map(({ node: n, x, y }) => {
            const Icon = KIND_ICON[n.kind];
            return (
              <button
                className="bakery-agent-node"
                data-agent-node
                data-kind={n.kind}
                data-status={n.status}
                data-testid="agent-node"
                key={n.id}
                onClick={() => open(n)}
                style={{ left: x - CARD_W / 2, top: y, width: CARD_W, height: CARD_H }}
                title={n.note ? `${n.label} — ${n.note}` : n.label}
                type="button"
              >
                <span className="bakery-agent-node-head">
                  <Icon className="size-3.5 shrink-0" />
                  <span className="bakery-agent-node-label">{n.label}</span>
                  <span aria-hidden className="bakery-agent-node-dot" />
                </span>
                <span className="bakery-agent-node-line">{nodeLine(n)}</span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
