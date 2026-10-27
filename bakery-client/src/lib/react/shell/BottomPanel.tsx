// The bottom panel (studio references, section 1): the operator's
// terminals, as VS Code shows them, on two tabs. Host is this Mac's own
// shell, from home, wherever the operator is; the terminal button and ⌘J
// always open on it, and no agent ever sees it or reaches it (the node's
// `sys:host` stream). The second tab is the room of the place the operator
// is in (terminalPlace.ts): a project's (its main folder, under its rules),
// a workspace's or a thread's, named after it, and it follows every move.
// Commands an agent typed for the operator to run sit in that room too. The
// Agent terminal, every agent's commands read-only, is not here: it opens
// beside its thread from a command's row (AgentTerminal.tsx). The picked
// terminal's group fills the panel, side by side once split; the list on
// the right carries each terminal's state, its split and its kill. Drag a
// terminal from the list onto one showing to split it there. Terminals are
// the node's own and the window keeps each one live (terminalSession.ts):
// switching tabs or places, hiding the panel and the Floor only take the
// view away. A tab shown with no terminal of the operator's starts one.

import { type DragEvent, type ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import { Columns2Icon, ExternalLinkIcon, FolderIcon, LaptopIcon, MessageSquareIcon, PlusIcon, TerminalIcon, XIcon } from "lucide-react";
import { terminalClose, terminalCreate, terminalList, type TerminalInfo } from "$lib/api.generated";
import { node } from "$lib/node";
import { dropSession } from "$lib/terminalSession";
import type { PtyMode } from "$lib/terminalMode";
import { actorLabel } from "$lib/transcript";
import { ThreadNamesContext } from "../AgentPanel";
import { mountTerminal, type TerminalProps } from "./svelteTerminal";
import type { TerminalPlace } from "./terminalPlace";
import { said, startResize } from "./ui";
import { splitBeside, syncGroups } from "./viewModel";

const TERMINAL_MIME = "application/x-bakery-terminal";
/** The node's stream for this Mac's own shells. */
const HOST_STREAM = "sys:host";
/** The mode of a thread's read-only Agent terminal, which this panel never shows. */
const AGENT_MODE = "agent";
/** Each stream's splits, kept while the window lives. */
const kept = new Map<string, string[][]>();
/** Each stream's picked terminal, so coming back to a place shows the one left there. */
const pickedOn = new Map<string, string>();
/** Each stream's terminals as last listed, shown at once while they list again. */
const listedOn = new Map<string, TerminalInfo[]>();
/** A terminal an agent opened (one it typed a command into for the
 *  operator), as against the operator's own, from the app or a device. */
const isAgents = (t: TerminalInfo) => t.opened_by.startsWith("agent");
/** What the panel lists: every terminal but the Agent terminal, which opens beside its thread. */
const barTerminals = (all: TerminalInfo[]) => all.filter((t) => t.mode !== AGENT_MODE);
type Status = { live: boolean; text: string };
type Tab = "host" | "room";
type Side = "before" | "after";

function TerminalView(props: TerminalProps) {
  const box = useRef<HTMLDivElement>(null);
  const latest = useRef(props);
  latest.current = props;
  const { streamId, terminalId } = props;
  useEffect(() => {
    if (!box.current) return;
    const p = latest.current;
    return mountTerminal(box.current, {
      ...p,
      bare: true,
      keepAlive: true,
      onClosed: () => latest.current.onClosed(),
      onStatus: (s) => latest.current.onStatus?.(s),
    });
  }, [streamId, terminalId]);
  return <div className="h-full min-h-0" ref={box} />;
}

/** A terminal's state in its card: live, ended, or what its connection says. */
function StateBox({ t, status }: { t: TerminalInfo; status: Status | undefined }) {
  const box = "shrink-0 rounded px-1 font-bold text-[9.5px] leading-4";
  if (!t.live) return <span className={`${box} bg-(--bg-chip) text-(--fg-faint)`}>ENDED</span>;
  if (status && !status.live) {
    return (
      <span className={`${box} max-w-20 truncate bg-(--chip-warn-bg) text-(--chip-warn-fg)`} title={status.text}>
        {status.text}
      </span>
    );
  }
  return <span className={`${box} bg-(--chip-ok-bg) text-(--chip-ok-fg)`}>LIVE</span>;
}

const iconButton =
  "grid h-full w-[26px] place-items-center text-(--fg-2) hover:bg-(--bg-chip) hover:text-(--fg) disabled:opacity-40";
const cardButton = "grid size-[18px] shrink-0 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg)";

/** Terminals the shell asked to show (a prepared row's), by thread: the
 *  panel takes them, on the room tab, at once or when it next shows that
 *  thread. */
const wantedTerms = new Map<string, string[]>();
const wantingTerms = new Set<() => void>();

export function showTerminals(streamId: string, terminalIds: string[]) {
  if (terminalIds.length === 0) return;
  wantedTerms.set(streamId, terminalIds);
  for (const take of wantingTerms) take();
}

/** The room tab's mark: its kind's icon, or the workspace's own tile. */
function PlaceMark({ place, tile }: { place: TerminalPlace; tile: { color: string; mark: string } | null }) {
  if (place.kind === "workspace" && tile) {
    return (
      <span className="grid size-[15px] shrink-0 place-items-center rounded-[4px] font-bold text-(--fg-on-solid) text-[9.5px]" style={{ background: tile.color }}>
        {tile.mark}
      </span>
    );
  }
  const hue = place.kind === "project" ? "var(--hue-project)" : "var(--hue-threads)";
  const Icon = place.kind === "project" ? FolderIcon : MessageSquareIcon;
  return (
    <span
      className="grid size-4 shrink-0 place-items-center rounded-[4px]"
      style={{ background: `color-mix(in srgb, ${hue} 16%, transparent)`, boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${hue} 32%, transparent)`, color: hue }}
    >
      <Icon className="size-[11px]" />
    </span>
  );
}

const hueOf = (place: TerminalPlace, tile: { color: string } | null) =>
  place.kind === "workspace" && tile ? tile.color : place.kind === "project" ? "var(--hue-project)" : "var(--hue-threads)";

export function BottomPanel({
  place,
  tile,
  height,
  onHeight,
  onClose,
}: {
  /** The place on screen, whose room is the second tab: none, Host alone. */
  place: TerminalPlace | null;
  /** The workspace's own tile, for a workspace's room. */
  tile: { color: string; mark: string } | null;
  height: number;
  onHeight: (height: number) => void;
  onClose: () => void;
}) {
  const el = useRef<HTMLElement>(null);
  const names = useContext(ThreadNamesContext);
  // The panel opens on Host: it is made fresh each time it opens.
  const [tab, setTab] = useState<Tab>(() => (place && wantedTerms.has(place.stream) ? "room" : "host"));
  const showing: Tab = tab === "room" && place ? "room" : "host";
  const streamId = showing === "host" ? HOST_STREAM : (place?.stream ?? HOST_STREAM);
  const mode: PtyMode = showing === "host" ? "host" : (place?.mode ?? "host");
  const [terms, setTerms] = useState<TerminalInfo[]>(() => listedOn.get(streamId) ?? []);
  const [groups, setGroups] = useState<string[][]>(() => kept.get(streamId) ?? []);
  const [picked, setPicked] = useState<string | null>(() => pickedOn.get(streamId) ?? null);
  const [status, setStatus] = useState<Record<string, Status>>({});
  const [drop, setDrop] = useState<{ id: string; side: Side } | null>(null);
  const [error, setError] = useState("");
  const [ghostty, setGhostty] = useState(false);
  /** Streams this panel already started a terminal for, so it does once. */
  const started = useRef(new Set<string>());
  /** The terminals now listed, for a split made after an await. */
  const listed = useRef<string[]>([]);
  listed.current = terms.map((t) => t.terminal_id);

  const pick = useCallback(
    (id: string | null) => {
      setPicked(id);
      if (id) pickedOn.set(streamId, id);
    },
    [streamId],
  );

  const regroup = useCallback(
    (next: (gs: string[][]) => string[][]) =>
      setGroups((gs) => {
        const out = next(gs);
        kept.set(streamId, out);
        return out;
      }),
    [streamId],
  );

  const create = useCallback(
    async (beside?: string) => {
      setError("");
      try {
        const t = await terminalCreate({ stream_id: streamId, mode, name: null, command: null });
        setTerms((ts) => {
          const next = [...ts, t];
          listedOn.set(streamId, next);
          return next;
        });
        if (beside) regroup((gs) => splitBeside(syncGroups(gs, [...listed.current, t.terminal_id]), t.terminal_id, beside, "after"));
        pick(t.terminal_id);
      } catch (e) {
        setError(said(e));
      }
    },
    [streamId, mode, regroup, pick],
  );

  const load = useCallback(() => {
    terminalList({ stream_id: streamId }).then(
      (all) => {
        const list = barTerminals(all);
        listedOn.set(streamId, list);
        setTerms(list);
        setError("");
        // A tab opens on a terminal of the operator's own, made if none.
        if (!list.some((t) => !isAgents(t)) && !started.current.has(streamId)) {
          started.current.add(streamId);
          void create();
        }
      },
      (e) => setError(said(e)),
    );
  }, [streamId, create]);
  useEffect(() => {
    setTerms(listedOn.get(streamId) ?? []);
    setGroups(kept.get(streamId) ?? []);
    setPicked(pickedOn.get(streamId) ?? null);
    setStatus({});
    load();
  }, [load, streamId]);

  // A prepared row's terminals: on the room tab, listed again (the agent
  // may have just opened them), side by side in one group, the first picked.
  useEffect(() => {
    const take = () => {
      const room = place?.stream;
      const named = room ? wantedTerms.get(room) : undefined;
      if (!room || !named) return;
      if (showing !== "room") return setTab("room");
      wantedTerms.delete(room);
      load();
      regroup((gs) => [named, ...gs.map((g) => g.filter((id) => !named.includes(id))).filter((g) => g.length > 0)]);
      pick(named[0] ?? null);
    };
    take();
    wantingTerms.add(take);
    return () => {
      wantingTerms.delete(take);
    };
  }, [place?.stream, showing, load, regroup, pick]);

  // The tab not showing still counts its terminals, live.
  const [, recount] = useState(0);
  const other = showing === "host" ? place?.stream : HOST_STREAM;
  useEffect(() => {
    if (!other) return;
    let live = true;
    terminalList({ stream_id: other }).then(
      (all) => {
        if (!live) return;
        listedOn.set(other, barTerminals(all));
        recount((n) => n + 1);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [other]);

  useEffect(() => {
    node.ghosttyAvailable().then(
      (r) => setGhostty(r.available),
      () => setGhostty(false),
    );
  }, []);

  const byId = new Map(terms.map((t) => [t.terminal_id, t]));
  const shownGroups = syncGroups(groups, terms.map((t) => t.terminal_id));
  const current = picked && byId.has(picked) ? picked : (shownGroups[0]?.[0] ?? null);
  const shown = shownGroups.find((g) => current !== null && g.includes(current)) ?? [];

  const gone = (id: string) => {
    setTerms((ts) => {
      const next = ts.filter((t) => t.terminal_id !== id);
      listedOn.set(streamId, next);
      return next;
    });
    setPicked((p) => (p === id ? null : p));
  };
  async function kill(id: string) {
    setError("");
    try {
      await terminalClose({ stream_id: streamId, terminal_id: id });
      dropSession(streamId, id);
      gone(id);
    } catch (e) {
      setError(said(e));
    }
  }

  const carries = (e: DragEvent) => e.dataTransfer.types.includes(TERMINAL_MIME);
  const sideOf = (e: DragEvent<HTMLElement>): Side => {
    const r = e.currentTarget.getBoundingClientRect();
    return e.clientX - r.left < r.width / 2 ? "before" : "after";
  };

  const tabButton = (key: Tab, hue: string, mark: ReactNode, label: string, count: number, title?: string) => (
    <button
      aria-selected={showing === key}
      className={`flex min-w-0 max-w-72 items-center gap-[7px] px-3 transition-colors ${showing === key ? "font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
      key={key}
      onClick={() => setTab(key)}
      role="tab"
      style={
        showing === key
          ? { background: `linear-gradient(180deg, transparent 10%, color-mix(in srgb, ${hue} 15%, transparent))`, boxShadow: `inset 0 -2px 0 ${hue}` }
          : undefined
      }
      title={title}
      type="button"
    >
      {mark}
      <span className="min-w-0 truncate">{label}</span>
      <span className="font-normal text-(--fg-dim)">{count}</span>
    </button>
  );
  const roomCount = place ? (listedOn.get(place.stream) ?? []).length : 0;

  return (
    <section
      aria-label="Terminal"
      className="relative flex shrink-0 flex-col border-(--line) border-t bg-(--bg-rail)"
      data-testid="shell-bottom"
      ref={el}
      style={{ height }}
    >
      <div
        aria-hidden
        className="absolute inset-x-0 -top-1 z-10 h-2 cursor-row-resize"
        onPointerDown={(e) => startResize(e, el.current, -1, [120, 720], onHeight, "y")}
      />
      <div className="flex h-[30px] shrink-0 items-stretch border-(--line-row) border-b bg-(image:--grad-chrome) px-1.5 text-[11px]">
        <div aria-label="Terminals" className="flex min-w-0 items-stretch" role="tablist">
          {tabButton(
            "host",
            "var(--link)",
            <LaptopIcon className="size-3.5 shrink-0 text-(--link)" />,
            "Host",
            (listedOn.get(HOST_STREAM) ?? []).length,
          )}
          {place && (
            <>
              <span aria-hidden className="mx-[3px] my-2 w-px shrink-0 bg-(--line)" />
              {tabButton("room", hueOf(place, tile), <PlaceMark place={place} tile={tile} />, place.name, roomCount, place.name)}
            </>
          )}
        </div>
        {error && (
          <span className="ml-2 min-w-0 self-center truncate text-(--bad)" title={error}>
            {error}
          </span>
        )}
        <span className="flex-1" />
        {ghostty && showing === "room" && place?.kind !== "project" && (
          <button
            aria-label="Open in Ghostty"
            className={iconButton}
            onClick={() => void node.ghosttyOpen(streamId).catch((e: unknown) => setError(said(e)))}
            title="Open in Ghostty"
            type="button"
          >
            <ExternalLinkIcon className="size-3.5" />
          </button>
        )}
        <button aria-label="New terminal" className={iconButton} onClick={() => void create()} title="New terminal" type="button">
          <PlusIcon className="size-3.5" />
        </button>
        <button aria-label="Hide the panel" className={iconButton} onClick={onClose} title="Hide (⌘J)" type="button">
          <XIcon className="size-3.5" />
        </button>
      </div>
      <div className="flex min-h-0 flex-1">
        <div className="flex min-w-0 flex-1">
          {shown.map((id, i) => {
            const t = byId.get(id);
            if (!t) return null;
            const hint = drop?.id === id ? drop.side : null;
            return (
              <div
                className={`relative min-w-0 flex-1 ${i > 0 ? "border-(--line) border-l" : ""}`}
                key={id}
                onDragLeave={(e) => {
                  if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDrop(null);
                }}
                onDragOver={(e) => {
                  if (!carries(e)) return;
                  e.preventDefault();
                  const side = sideOf(e);
                  setDrop((d) => (d?.id === id && d.side === side ? d : { id, side }));
                }}
                onDrop={(e) => {
                  const moved = e.dataTransfer.getData(TERMINAL_MIME);
                  setDrop(null);
                  if (!moved) return;
                  e.preventDefault();
                  const side = sideOf(e);
                  regroup((gs) => splitBeside(syncGroups(gs, [...byId.keys()]), moved, id, side));
                  pick(moved);
                }}
                onPointerDownCapture={() => pick(id)}
                style={{
                  boxShadow: hint ? `inset ${hint === "before" ? 3 : -3}px 0 0 var(--accent)` : undefined,
                }}
              >
                <TerminalView
                  name={t.name}
                  onClosed={() => gone(id)}
                  onStatus={(s) => setStatus((all) => ({ ...all, [id]: s }))}
                  readOnly={false}
                  streamId={t.stream_id}
                  terminalId={id}
                />
              </div>
            );
          })}
        </div>
        {terms.length > 0 && (
          <ul
            aria-label="Terminals"
            className="flex w-[230px] shrink-0 flex-col gap-px overflow-y-auto border-(--line-row) border-l bg-(--bg-tabs) p-1 text-[11px]"
          >
            {shownGroups.flatMap((g) =>
              g.map((id, i) => {
                const t = byId.get(id);
                if (!t) return null;
                const on = id === current;
                const opener = actorLabel(t.opened_by, names);
                return (
                  <li className="flex items-center" key={id}>
                    {g.length > 1 && (
                      <span aria-hidden className="w-3 shrink-0 font-mono text-(--fg-faint) leading-none">
                        {i === 0 ? "┌" : i === g.length - 1 ? "└" : "├"}
                      </span>
                    )}
                    <div
                      className={`flex h-7 min-w-0 flex-1 cursor-grab items-center gap-1 rounded pr-1 pl-2 ${on ? "bg-(--accent-sel) text-(--fg) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg-2)"}`}
                      draggable
                      onDragStart={(e) => {
                        e.dataTransfer.setData(TERMINAL_MIME, id);
                        e.dataTransfer.effectAllowed = "move";
                      }}
                    >
                      <button
                        className="flex h-full min-w-0 flex-1 items-center gap-[7px] text-left"
                        onClick={() => pick(id)}
                        title={`${t.cwd} · ${t.mode} · opened by ${opener}`}
                        type="button"
                      >
                        <TerminalIcon className="size-3 shrink-0 text-(--ok)" />
                        <span className="min-w-0 flex-auto truncate">{t.name}</span>
                        {t.opened_by.split(":").length > 2 && <span className="min-w-0 shrink-[3] truncate text-(--fg-faint)">{opener}</span>}
                      </button>
                      <StateBox status={status[id]} t={t} />
                      <button
                        aria-label={`Split ${t.name}`}
                        className={cardButton}
                        onClick={() => void create(id)}
                        title="Split"
                        type="button"
                      >
                        <Columns2Icon className="size-3" />
                      </button>
                      <button
                        aria-label={`Kill ${t.name}`}
                        className={`${cardButton} hover:text-(--bad)`}
                        onClick={() => void kill(id)}
                        title="Kill this terminal"
                        type="button"
                      >
                        <XIcon className="size-3" />
                      </button>
                    </div>
                  </li>
                );
              }),
            )}
          </ul>
        )}
      </div>
    </section>
  );
}
