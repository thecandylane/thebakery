// A conversation's standing brief (Round 11's B1): pinned under its header
// and given to its agent every turn. The Baker, or an orchestrator in the
// same project, sets one directly, in effect until the operator keeps it or
// turns it down (the one before then holds again); the operator writes
// their own, changes it or clears it. An agent never sets its own.

import { useContext, useEffect, useState } from "react";
import { MessageResponse } from "@/lib/ai-elements/message";
import { type Brief, briefSet, conversationBrief, conversationBriefDecide } from "$lib/api.generated";
import { ago } from "$lib/conversations";
import { onEvent } from "$lib/events";
import { actorLabel } from "$lib/transcript";
import { ThreadNamesContext } from "../AgentPanel";
import { keep, useCached } from "./cache";
import { PLAIN, SOLID } from "./Internet";
import { said } from "./ui";

/** A conversation's brief, kept like a panel's data and read again as it
 *  changes; `has` is a brief to show, `available` a node that has briefs. */
export function useBrief(streamId: string) {
  const key = `brief:${streamId}`;
  const c = useCached<Brief>(key, () => conversationBrief({ stream_id: streamId }));
  const { reload } = c;
  useEffect(
    () =>
      onEvent(streamId, (ev) => {
        if (ev.family === "brief.set" || ev.family === "brief.decided") void reload();
      }),
    [streamId, reload],
  );
  return {
    key,
    brief: c.data,
    available: !c.error.includes("has no route for"),
    has: !!c.data && (c.data.text.trim() !== "" || c.data.pending),
  };
}

export function BriefBar({ streamId, b, onClose }: { streamId: string; b: ReturnType<typeof useBrief>; onClose: () => void }) {
  const names = useContext(ThreadNamesContext);
  const brief = b.brief;
  const current = brief?.current ?? null;
  const empty = !brief || brief.text.trim() === "";
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [history, setHistory] = useState(false);
  const writing = editing || (empty && !brief?.pending);
  const who = (by: string) => (by === "human" ? "you" : actorLabel(by, names));
  const done = (next: Brief) => {
    keep(b.key, next);
    setBusy(false);
    setEditing(false);
  };
  const failed = (e: unknown) => {
    setBusy(false);
    setError(said(e));
  };
  const save = (text: string) => {
    setBusy(true);
    setError("");
    briefSet({ stream_id: streamId, text }).then(done, failed);
  };
  const decide = (keepIt: boolean) => {
    if (!current) return;
    setBusy(true);
    setError("");
    conversationBriefDecide({ stream_id: streamId, event_hash: current.event_hash, keep: keepIt }).then(done, failed);
  };
  const edit = () => {
    setDraft(brief?.text ?? "");
    setEditing(true);
  };
  return (
    <div className="flex shrink-0 flex-col border-(--line) border-b bg-(--bg-raised)" data-testid="brief" style={{ boxShadow: "inset 3px 0 0 var(--violet)" }}>
      <div className="flex items-start gap-3 px-3 py-2">
        <div className="flex w-14 shrink-0 flex-col gap-0.5 pt-0.5">
          <span className="font-bold text-(--violet) text-[10px] tracking-[.08em]">BRIEF</span>
          <span className="text-(--fg-faint) text-[10px]">every turn</span>
        </div>
        {writing ? (
          <textarea
            aria-label="Brief"
            autoFocus
            className="h-20 min-w-0 flex-1 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 py-1 text-(--fg-body) text-xs"
            onChange={(e) => setDraft(e.target.value)}
            placeholder="Its standing brief"
            value={draft}
          />
        ) : (
          <div className="max-h-32 min-w-0 flex-1 overflow-auto text-xs">
            <MessageResponse className="bakery-ai-prose">{brief?.text ?? ""}</MessageResponse>
          </div>
        )}
        <div className="flex shrink-0 flex-col items-end gap-1">
          {writing ? (
            <span className="flex gap-1">
              {!empty && (
                <button className={PLAIN} disabled={busy} onClick={() => save("")} title="Clear the brief" type="button">
                  Clear
                </button>
              )}
              <button className={PLAIN} onClick={() => (empty ? onClose() : setEditing(false))} type="button">
                Cancel
              </button>
              <button className={SOLID} disabled={busy || draft.trim() === (brief?.text ?? "").trim()} onClick={() => save(draft)} type="button">
                Save
              </button>
            </span>
          ) : (
            <button className={PLAIN} onClick={edit} type="button">
              Edit
            </button>
          )}
          {current && (
            <span className="text-(--fg-dim) text-[10.5px]">
              by {who(current.by)} · {ago(current.at)}
              {brief && brief.history.length > 1 && (
                <>
                  {" · "}
                  <button className="text-(--link) hover:underline" onClick={() => setHistory((h) => !h)} type="button">
                    {brief.history.length} changes
                  </button>
                </>
              )}
            </span>
          )}
        </div>
      </div>
      {brief?.pending && current && (
        <div className="flex items-center gap-2 border-(--line-soft) border-t bg-(--warn-bg) px-3 py-1.5" data-testid="brief-pending">
          <span className="font-bold text-(--warn) text-[10px] tracking-[.08em]">{who(current.by).toUpperCase()} SET THIS BRIEF</span>
          <span className="flex-1" />
          <button className={PLAIN} disabled={busy} onClick={() => decide(false)} type="button">
            Turn down
          </button>
          <button className={SOLID} disabled={busy} onClick={() => decide(true)} type="button">
            Keep
          </button>
        </div>
      )}
      {history && brief && (
        <ol className="flex max-h-40 flex-col gap-1 overflow-auto border-(--line-soft) border-t px-3 py-1.5 text-[11px]">
          {brief.history.map((h) => (
            <li className="flex min-w-0 gap-2" key={h.event_hash}>
              <span className="w-40 shrink-0 truncate text-(--fg-dim)">
                {who(h.by)} · {ago(h.at)}
                {h.decision ? ` · ${h.decision}` : ""}
              </span>
              <span className="min-w-0 flex-1 truncate text-(--fg-body)" title={h.text}>
                {h.text.trim() || "cleared"}
              </span>
            </li>
          ))}
        </ol>
      )}
      {error && <p className="px-3 pb-1.5 text-(--bad) text-[11px]">{error}</p>}
    </div>
  );
}
