// Saved (Round 10): a reply's Save beside its Copy, and the Saved panel in a
// thread's pane: that thread's saved replies, or every thread's, with their
// checklists ticked in place. Only the operator sees any of it: the node
// keeps it outside every thread, no agent can read it, and nothing here
// sends it anywhere.

import { useMemo, useState } from "react";
import { BookmarkIcon, LockIcon, XIcon } from "lucide-react";
import { MessageAction, MessageResponse } from "@/lib/ai-elements/message";
import { ago } from "$lib/conversations";
import { bakeryCodePlugin } from "../highlight";
import { savedBody, savedParts, savedTitle } from "../savedModel";
import { type SavedReply, saveReply, tickItem, unsaveReply, useSaved } from "./saved";
import { said } from "./ui";

const PLUGINS = { code: bakeryCodePlugin };
const CONTROLS = { code: { copy: true, download: false }, table: false } as const;
const SEG = (on: boolean) => `h-[22px] px-2 text-[10.5px] ${on ? "bg-(--bg-pressed) font-bold text-(--accent-fg)" : "text-(--fg-2) hover:text-(--fg)"}`;

const Markdown = ({ text }: { text: string }) => (
  <MessageResponse className="bakery-ai-prose" controls={CONTROLS} lineNumbers={false} mode="static" plugins={PLUGINS}>
    {text}
  </MessageResponse>
);

/** Save, beside a reply's Copy: filled once saved, and a click unsaves it. */
export function SaveReply({ streamId, hash }: { streamId: string; hash: string }) {
  const saved = useSaved();
  const mine = saved.items.find((i) => i.event_hash === hash) ?? null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const toggle = () => {
    setBusy(true);
    setError("");
    (mine ? unsaveReply(mine.id) : saveReply(streamId, hash)).then(
      () => setBusy(false),
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <MessageAction disabled={busy} label={mine ? "Unsave reply" : "Save reply"} onClick={toggle} size="icon-xs" tooltip={error || (mine ? "Saved: only you can see it" : "Save")}>
      <BookmarkIcon className={`size-3.5 ${mine ? "fill-current text-(--accent)" : ""} ${error ? "text-(--bad)" : ""}`} />
    </MessageAction>
  );
}

function SavedCard({ r, thread, onOpen }: { r: SavedReply; thread: string | null; onOpen: (() => void) | null }) {
  const [error, setError] = useState("");
  const parts = useMemo(() => savedParts(savedBody(r.text)), [r.text]);
  const items = parts.filter((p) => p.kind === "item").length;
  const done = new Set(r.done);
  const ticked = parts.filter((p) => p.kind === "item" && done.has(p.index)).length;
  const fail = (e: unknown) => setError(said(e));
  return (
    <section className="flex flex-col gap-1.5 rounded-lg border border-(--line) bg-(--bg-raised) p-2.5" data-testid="saved-reply">
      <div className="flex min-w-0 items-center gap-1.5">
        <BookmarkIcon className="size-3 shrink-0 fill-current text-(--accent)" />
        <b className="min-w-0 flex-1 truncate text-(--fg)">{savedTitle(r.text)}</b>
        <span className="shrink-0 text-(--fg-dim) text-[10.5px]">{ago(r.saved_at)}</span>
        <button
          aria-label="Unsave"
          className="grid size-5 shrink-0 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--bad)"
          onClick={() => void unsaveReply(r.id).catch(fail)}
          title="Unsave"
          type="button"
        >
          <XIcon className="size-3" />
        </button>
      </div>
      {thread && <span className="truncate text-(--fg-dim) text-[10.5px]">{thread}</span>}
      {items > 0 && (
        <div className="flex items-center gap-2">
          <span className="h-1 flex-1 overflow-hidden rounded-full bg-(--bg-chip)">
            <span className="block h-full rounded-full bg-(--ok)" style={{ width: `${Math.round((ticked / items) * 100)}%` }} />
          </span>
          <span className="shrink-0 text-(--fg-dim) text-[10.5px]">
            {ticked}/{items}
          </span>
        </div>
      )}
      <div className="flex flex-col gap-1 text-[11.5px] text-(--fg-body)">
        {parts.map((p) =>
          p.kind === "text" ? (
            <Markdown key={`t${p.text.length}:${p.text.slice(0, 24)}`} text={p.text} />
          ) : (
            <label className="flex cursor-pointer items-start gap-2" key={`i${p.index}`} style={{ paddingLeft: p.depth * 14 }}>
              <input
                checked={done.has(p.index)}
                className="mt-[3px] shrink-0 accent-(--ok)"
                onChange={(e) => void tickItem(r.id, p.index, e.target.checked).catch(fail)}
                type="checkbox"
              />
              <span className={`min-w-0 flex-1 ${done.has(p.index) ? "text-(--fg-faint) line-through" : ""}`}>
                <Markdown text={p.text} />
              </span>
            </label>
          ),
        )}
      </div>
      {onOpen && (
        <button className="self-end text-(--link) text-[10.5px] hover:underline" onClick={onOpen} type="button">
          Open in thread ›
        </button>
      )}
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </section>
  );
}

/** A thread's saved replies, or (All threads) every one, newest first. */
export function SavedPanel({
  streamId,
  threadOf,
  onClose,
}: {
  streamId: string;
  /** A thread's name and side, for a reply saved elsewhere. */
  threadOf: (streamId: string) => { title: string; bakerThread: boolean } | null;
  onClose: () => void;
}) {
  const saved = useSaved();
  const [all, setAll] = useState(false);
  const shown = all ? saved.items : saved.items.filter((i) => i.stream_id === streamId);
  const open = (id: string) =>
    window.dispatchEvent(new CustomEvent("bakery:open-conversation", { detail: { streamId: id, bakerThread: threadOf(id)?.bakerThread ?? false } }));
  return (
    <aside aria-label="Saved" className="flex w-80 shrink-0 flex-col border-(--line) border-l bg-(--bg-panel) text-xs" data-testid="saved-panel">
      <div className="flex h-9 shrink-0 items-center gap-2 border-(--line-soft) border-b bg-(--bg-raised) pr-1.5 pl-2.5">
        <BookmarkIcon className="size-3.5 fill-current text-(--accent)" />
        <span className="font-bold text-(--accent) text-[10.5px] tracking-[.08em]">SAVED</span>
        <span className="flex-1" />
        <span aria-label="Which saved replies" className="flex overflow-hidden rounded-[5px] border border-(--line)" role="radiogroup">
          <button aria-checked={!all} className={SEG(!all)} onClick={() => setAll(false)} role="radio" type="button">
            This thread
          </button>
          <button aria-checked={all} className={`${SEG(all)} border-(--line) border-l`} onClick={() => setAll(true)} role="radio" type="button">
            All threads
          </button>
        </span>
        <button aria-label="Close Saved" className="grid size-6 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg)" onClick={onClose} type="button">
          <XIcon className="size-3.5" />
        </button>
      </div>
      <p className="flex items-center gap-1.5 border-(--line-soft) border-b px-2.5 py-1 text-(--fg-dim) text-[10.5px]">
        <LockIcon className="size-3 shrink-0" />
        Only you can see this
      </p>
      <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-auto p-2">
        {shown.map((r) => (
          <SavedCard
            key={r.id}
            onOpen={r.stream_id === streamId ? null : () => open(r.stream_id)}
            r={r}
            thread={all ? (threadOf(r.stream_id)?.title ?? "A thread no longer here") : null}
          />
        ))}
        {shown.length === 0 && <p className="px-1 py-2 text-(--fg-faint)">Nothing saved yet.</p>}
        {saved.error && <p className="px-1 text-(--bad) text-[11px]">{saved.error}</p>}
      </div>
    </aside>
  );
}
