// The bar in the middle of the top bar (⌘K), as the operator asked for it:
// what the Bakery can do first — its commands, then every thread — and
// under that what is live in the workspace on screen, to focus or open,
// then the text in its files (⌘⇧F opens on those alone). VS Code's command
// bar is the model: one input, the list filters as you type, Enter runs.
// Every entry does something real. A secret file's line shows masked until
// it is clicked open, and what it reveals stays on screen: never in the
// history or a toast.

import { type ReactNode, useEffect, useState } from "react";
import { FileTextIcon } from "lucide-react";
import {
  Command,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
  CommandShortcut,
} from "@/components/ui/command";
import { type Found2 as Found, type Revealed, type Scope2, search, searchReveal } from "$lib/api.generated";
import { statusLabel } from "$lib/conversations";
import { said, StatusDot, useSecretShown } from "./ui";

export type BarCommand = { id: string; label: string; hint?: string; run: () => void };
/** A thread or panel to go to: its name, where it is, and its state; for a
 *  thread, its stream, so its glyph shows its live phase. */
export type BarItem = { id: string; title: string; place: string; status: string | null; streamId?: string; icon?: ReactNode; run: () => void };
/** Text search: the thread whose folder "This workspace" searches (null: only
 *  everywhere), where a match opens (with the thread it was searched from),
 *  and whether the bar opened on it alone (⌘⇧F). */
export type BarFiles = { streamId: string | null; open: (m: Found, from: string | null) => void; only: boolean };

const SEG = "h-5 px-2 text-[11px] disabled:opacity-40";
const SEG_ON = "bg-(--bg-pressed) font-bold text-(--accent-fg)";
const matchKey = (m: Found) => `${m.folder}\n${m.path}\n${m.line ?? 0}`;

/** The node's matches for `q` (from two characters on, a moment after the
 *  last key), "searching" while they come, or null for no search. */
function useTextSearch(q: string, scope: Scope2, exact: boolean, streamId: string | null) {
  const text = q.trim();
  const key = JSON.stringify([text, scope, exact, streamId]);
  const [got, setGot] = useState<{ key: string; matches: Found[]; truncated: boolean; error: string } | null>(null);
  useEffect(() => {
    if (text.length < 2) return;
    let live = true;
    const t = setTimeout(() => {
      search({ q: text, scope, stream_id: scope === "workspace" ? streamId : null, exact_case: exact }).then(
        (r) => live && setGot({ key, matches: r.matches, truncated: r.truncated, error: "" }),
        (e) => live && setGot({ key, matches: [], truncated: false, error: said(e) }),
      );
    }, 250);
    return () => {
      live = false;
      clearTimeout(t);
    };
  }, [key, text, scope, exact, streamId]);
  if (text.length < 2) return null;
  return got?.key === key ? got : "searching";
}

export function CommandBar({
  onClose,
  commands,
  threads,
  here,
  hereName,
  files,
}: {
  onClose: () => void;
  commands: readonly BarCommand[];
  /** Threads anywhere in the Bakery. */
  threads: readonly BarItem[];
  /** What is live in the workspace on screen: its panels and its threads. */
  here: readonly BarItem[];
  hereName: string;
  files: BarFiles;
}) {
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState<Scope2>(files.streamId ? "workspace" : "everywhere");
  const [exact, setExact] = useState(false);
  const found = useTextSearch(query, scope, exact, files.streamId);
  const matches = found && found !== "searching" ? found.matches : [];
  /** Masked lines opened by a click, by match: on screen only. */
  const [revealed, setRevealed] = useState<ReadonlyMap<string, Revealed>>(new Map());
  useSecretShown(matches.some((m) => revealed.get(matchKey(m))?.secret === true));
  const [revealError, setRevealError] = useState("");
  const reveal = (m: Found) => {
    setRevealError("");
    searchReveal({ scope, stream_id: scope === "workspace" ? files.streamId : null, folder: m.folder, path: m.path, line: m.line ?? 0 }).then(
      (r) => setRevealed((all) => new Map(all).set(matchKey(m), r)),
      (e) => setRevealError(said(e)),
    );
  };
  useEffect(() => {
    const away = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("keydown", away);
    return () => document.removeEventListener("keydown", away);
  }, [onClose]);
  const pick = (run: () => void) => () => {
    onClose();
    run();
  };
  const item = (it: BarItem) => (
    <CommandItem key={it.id} keywords={[it.place]} onSelect={pick(it.run)} value={`${it.id} ${it.title}`}>
      {it.status ? <StatusDot status={it.status} streamId={it.streamId} title={statusLabel(it.status as never)} /> : it.icon}
      <span className="min-w-0 flex-1 truncate">{it.title}</span>
      {it.place && <span className="shrink-0 truncate text-(--fg-dim) text-[11px]">{it.place}</span>}
    </CommandItem>
  );

  return (
    <div className="fixed inset-0 z-50" data-testid="command-bar" onPointerDown={onClose}>
      {/* A dialog, so Code and Preview views (which paint above the page)
          hide while it is open instead of covering it. */}
      <div
        aria-label="Search and commands"
        aria-modal="true"
        className="absolute top-[36px] left-1/2 w-[min(640px,92vw)] -translate-x-1/2 overflow-hidden rounded-md border border-(--line-strong) bg-(--bg-raised) shadow-(--shadow-menu)"
        onPointerDown={(e) => e.stopPropagation()}
        role="dialog"
      >
        <Command loop>
          <CommandInput
            autoFocus
            onValueChange={setQuery}
            placeholder={files.only ? "Text in files…" : "Commands, threads, panels, text in files…"}
            value={query}
          />
          {/* Pressing these keeps the keyboard in the input. */}
          <div className="flex items-center gap-2 border-(--line-soft) border-b px-3 py-1 text-(--fg-dim) text-[11px]" onMouseDown={(e) => e.preventDefault()}>
            <span>In files</span>
            <span aria-label="Where to search" className="flex overflow-hidden rounded-[5px] border border-(--line)" role="radiogroup">
              <button
                aria-checked={scope === "workspace"}
                className={`${SEG} ${scope === "workspace" ? SEG_ON : ""}`}
                disabled={!files.streamId}
                onClick={() => setScope("workspace")}
                role="radio"
                type="button"
              >
                This workspace
              </button>
              <button
                aria-checked={scope === "everywhere"}
                className={`${SEG} border-(--line) border-l ${scope === "everywhere" ? SEG_ON : ""}`}
                onClick={() => setScope("everywhere")}
                role="radio"
                type="button"
              >
                Everywhere
              </button>
            </span>
            <button
              aria-label="Match case"
              aria-pressed={exact}
              className={`h-5 rounded-[5px] border border-(--line) px-1.5 font-bold text-[11px] ${exact ? SEG_ON : ""}`}
              onClick={() => setExact((v) => !v)}
              title="Match case"
              type="button"
            >
              Aa
            </button>
            <span className="flex-1" />
            {found === "searching" && <span>Searching…</span>}
            {found && found !== "searching" && !found.error && (
              <span>{found.truncated ? `first ${matches.length.toLocaleString()}` : `${matches.length.toLocaleString()} found`}</span>
            )}
          </div>
          <CommandList className="max-h-[min(60vh,520px)]">
            {matches.length === 0 && (query.trim().length > 0 || !files.only) && found !== "searching" && <CommandEmpty>No matches</CommandEmpty>}
            {!files.only && (
              <>
                <CommandGroup heading="Bakery">
                  {commands.map((c) => (
                    <CommandItem key={c.id} onSelect={pick(c.run)} value={`${c.id} ${c.label}`}>
                      <span className="min-w-0 flex-1 truncate">{c.label}</span>
                      {c.hint && <CommandShortcut>{c.hint}</CommandShortcut>}
                    </CommandItem>
                  ))}
                </CommandGroup>
                {threads.length > 0 && <CommandGroup heading="Threads">{threads.map(item)}</CommandGroup>}
                {here.length > 0 && <CommandGroup heading={hereName}>{here.map(item)}</CommandGroup>}
              </>
            )}
            {found && found !== "searching" && found.error && <p className="px-3 py-2 text-(--bad) text-xs">{found.error}</p>}
            {revealError && <p className="px-3 py-1 text-(--bad) text-xs">{revealError}</p>}
            {matches.length > 0 && (
              <CommandGroup forceMount heading={scope === "workspace" ? "In this workspace's files" : "In files everywhere"}>
                {matches.map((m, i) => {
                  const shown = revealed.get(matchKey(m))?.text;
                  return (
                    <CommandItem
                      forceMount
                      key={`${matchKey(m)}\n${i}`}
                      onSelect={pick(() => files.open(m, scope === "workspace" ? files.streamId : null))}
                      value={`file ${i} ${m.path}`}
                    >
                      <FileTextIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
                      <span className="min-w-0 flex-1 truncate">
                        <span className="text-(--fg)">{m.path}</span>
                        {m.line ? <span className="text-(--fg-dim)">:{m.line}</span> : null}
                        {m.text && <span className="ml-2 font-mono text-(--fg-2) text-[11px]">{shown ?? m.text}</span>}
                      </span>
                      {m.masked && shown === undefined && (
                        <button
                          className="shrink-0 rounded px-1 text-(--link) text-[11px] hover:bg-(--bg-chip)"
                          onClick={(e) => {
                            e.stopPropagation();
                            reveal(m);
                          }}
                          title="Show the whole line"
                          type="button"
                        >
                          Show
                        </button>
                      )}
                      {scope === "everywhere" && <span className="max-w-[30%] shrink-0 truncate text-(--fg-dim) text-[11px]">{m.name}</span>}
                    </CommandItem>
                  );
                })}
              </CommandGroup>
            )}
          </CommandList>
        </Command>
      </div>
    </div>
  );
}
