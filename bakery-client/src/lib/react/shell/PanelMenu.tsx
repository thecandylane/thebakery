// "+ Panel" at the end of a tab bar: opens Merge review or Containers in this
// workspace, or adds an agent to its project. A new agent gets its own
// worktree; sharing the shown thread's folder is the explicit other choice,
// never offered on the Bakery's own repo.

import { type RefObject, useEffect, useRef, useState } from "react";
import { BotIcon, BoxIcon, GitMergeIcon, PlusIcon } from "lucide-react";
import type { ToolsLimit } from "$lib/api.generated";
import type { AgentPlace } from "./sidebarModel";
import { said } from "./ui";

export type NewAgent = { name: string; repo: string; shared: string | null; tools: ToolsLimit };

const LIMITS: { value: ToolsLimit; label: string }[] = [
  { value: "all", label: "All" },
  { value: "read", label: "Read" },
  { value: "none", label: "None" },
];

export const POP =
  "absolute top-full right-0 z-30 mt-1 rounded-md border border-(--line-strong) bg-(--bg-raised) p-1 text-xs shadow-(--shadow-menu)";
const SEG = "flex overflow-hidden rounded-[5px] border border-(--line)";
const SEG_BUTTON = "h-[22px] px-2 text-[11px] text-(--fg-2) hover:text-(--fg)";
const SEG_ON = "bg-(--bg-pressed) font-bold text-(--accent-fg)";

/** Closes a popover on Escape or a press outside `box`. */
export function useDismiss(open: boolean, box: RefObject<HTMLElement | null>, close: () => void) {
  const latest = useRef(close);
  latest.current = close;
  useEffect(() => {
    if (!open) return;
    const away = (e: Event) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !box.current?.contains(e.target as Node)) latest.current();
    };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", away);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", away);
    };
  }, [open, box]);
}

export function AddAgentForm({
  place,
  create,
  onDone,
}: {
  place: AgentPlace;
  create: (agent: NewAgent) => Promise<void>;
  onDone: () => void;
}) {
  const [name, setName] = useState("");
  const [repo, setRepo] = useState(place.repo);
  const [shared, setShared] = useState(false);
  const [tools, setTools] = useState<ToolsLimit>("all");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <form
      aria-label="Add agent"
      className="flex flex-col gap-1.5 p-1"
      onSubmit={(e) => {
        e.preventDefault();
        const n = name.trim();
        if (!n || busy) return;
        setBusy(true);
        setError("");
        create({ name: n, repo, shared: shared && place.shared ? place.shared.path : null, tools }).then(onDone, (err) => {
          setError(said(err));
          setBusy(false);
        });
      }}
    >
      <input
        aria-label="Agent name"
        autoFocus
        className="w-full rounded border border-(--line) bg-(--bg-app) px-1.5 py-1 text-xs outline-none focus:border-(--accent)"
        disabled={busy}
        onChange={(e) => setName(e.target.value)}
        placeholder="Agent name"
        value={name}
      />
      {place.shared && (
        <div aria-label="Where it works" className={SEG} role="radiogroup">
          <button aria-checked={!shared} className={`${SEG_BUTTON} shrink-0 ${shared ? "" : SEG_ON}`} onClick={() => setShared(false)} role="radio" type="button">
            Own worktree
          </button>
          <button
            aria-checked={shared}
            className={`${SEG_BUTTON} min-w-0 flex-1 truncate border-(--line) border-l ${shared ? SEG_ON : ""}`}
            onClick={() => setShared(true)}
            role="radio"
            title={place.shared.path}
            type="button"
          >
            Shared folder · {place.shared.branch || place.shared.path.split("/").pop()}
          </button>
        </div>
      )}
      {!shared && place.repos.length > 1 && (
        <select
          aria-label="Repository"
          className="rounded border border-(--line) bg-(--bg-app) px-1 py-0.5 text-(--fg-2) text-xs"
          onChange={(e) => setRepo(e.target.value)}
          value={repo}
        >
          {place.repos.map((r) => (
            <option key={r.root} value={r.root}>
              {r.name}
            </option>
          ))}
        </select>
      )}
      <div className="flex items-center gap-2">
        <span className="text-(--fg-dim) text-[11px]">Tools</span>
        <div aria-label="Tools" className={`${SEG} flex-1`} role="radiogroup">
          {LIMITS.map((t, i) => (
            <button
              aria-checked={tools === t.value}
              className={`${SEG_BUTTON} flex-1 ${i > 0 ? "border-(--line) border-l" : ""} ${tools === t.value ? SEG_ON : ""}`}
              key={t.value}
              onClick={() => setTools(t.value)}
              role="radio"
              type="button"
            >
              {t.label}
            </button>
          ))}
        </div>
      </div>
      <button
        className="h-6 self-end rounded-[5px] border border-(--accent-fg) bg-(image:--grad-accent) px-3 font-bold text-(--fg-on-solid) text-[11px] disabled:opacity-50"
        disabled={busy || !name.trim()}
        type="submit"
      >
        Add
      </button>
      {error && <p className="px-0.5 text-(--bad) text-[11px]">{error}</p>}
    </form>
  );
}

export function PanelMenu({
  onOpen,
  addAgent,
}: {
  onOpen: (kind: "review" | "containers") => void;
  /** Null when this workspace has no project to add an agent to. */
  addAgent: { place: AgentPlace; create: (agent: NewAgent) => Promise<void> } | null;
}) {
  const [open, setOpen] = useState<"menu" | "agent" | null>(null);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open !== null, box, () => setOpen(null));
  const item = "flex h-7 w-full items-center gap-2 rounded px-2 text-left text-(--fg-body) hover:bg-(--bg-chip)";
  const pick = (kind: "review" | "containers") => {
    setOpen(null);
    onOpen(kind);
  };
  return (
    <div className="relative" ref={box}>
      <button
        aria-expanded={open !== null}
        aria-haspopup="menu"
        className={`${SEG} h-6 items-center gap-[5px] bg-(image:--grad-button) px-2 text-(--fg-2) text-[11px] hover:text-(--fg)`}
        onClick={() => setOpen((o) => (o ? null : "menu"))}
        type="button"
      >
        <PlusIcon className="size-3 text-(--accent)" />
        Panel
      </button>
      {open === "menu" && (
        <div className={`${POP} w-48`} role="menu">
          <button className={item} onClick={() => pick("review")} role="menuitem" type="button">
            <GitMergeIcon className="size-3.5 text-(--accent)" />
            Merge review
          </button>
          <button className={item} onClick={() => pick("containers")} role="menuitem" type="button">
            <BoxIcon className="size-3.5 text-(--hue-containers)" />
            Containers
          </button>
          {addAgent && (
            <button className={item} onClick={() => setOpen("agent")} role="menuitem" type="button">
              <BotIcon className="size-3.5 text-(--ok)" />
              Add agent
            </button>
          )}
        </div>
      )}
      {open === "agent" && addAgent && (
        <div aria-label="Add agent" className={`${POP} w-80`} role="dialog">
          <AddAgentForm create={addAgent.create} onDone={() => setOpen(null)} place={addAgent.place} />
        </div>
      )}
    </div>
  );
}
