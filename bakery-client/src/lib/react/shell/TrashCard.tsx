// The Bakery's Trash, on its own control plane: the unsaved work deletes
// kept in git, newest first, each until the operator empties it. Put back
// makes its branch and a worktree again, its changes uncommitted, as they
// were. Empty takes one entry, or all the card shows, after asking.

import { useCallback, useState } from "react";
import { ArchiveIcon, ArchiveRestoreIcon, Trash2Icon } from "lucide-react";
import { trash, trashEmpty, trashPutBack, type TrashEntry } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { Card } from "./ControlCards";
import { Changes } from "./DeleteDialog";
import { PLAIN } from "./Internet";
import { said, usePoll } from "./ui";

const EMPTY =
  "flex h-6 shrink-0 items-center gap-1 rounded-[5px] border border-(--bad-border) px-2.5 text-(--bad) text-[11px] hover:bg-(--bad-bg) disabled:opacity-50";
const tilde = (p: string) => p.replace(/^\/(Users|home)\/[^/]+(?=\/|$)/, "~");
const keyOf = (e: TrashEntry) => `${e.repo}:${e.name}:${e.stamp}`;
/** Which entry is being put back or emptied, or "all" of them. */
type Busy = { what: "back" | "empty"; key: string };
/** When an entry was kept, from its stamp (`20261014T120000Z`, UTC). */
function keptAt(stamp: string): Date | null {
  const m = stamp.match(/^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})(\d{2})Z$/);
  return m ? new Date(Date.UTC(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6])) : null;
}

export function TrashCard() {
  const [entries, setEntries] = useState<TrashEntry[]>([]);
  const [busy, setBusy] = useState<Busy | null>(null);
  const [error, setError] = useState("");
  /** The last entry put back, and where. */
  const [back, setBack] = useState("");
  /** Entries the last Empty couldn't take, and why. */
  const [refused, setRefused] = useState("");
  const load = useCallback(
    () =>
      void trash().then(
        (t) => {
          setEntries(t.entries);
          setError("");
        },
        (e) => setError(said(e)),
      ),
    [],
  );
  usePoll(load, 30_000);
  const putBack = (e: TrashEntry) => {
    setBusy({ what: "back", key: keyOf(e) });
    setError("");
    setRefused("");
    trashPutBack({ repo: e.repo, name: e.name, stamp: e.stamp })
      .then(
        (b) => {
          setBack(`${b.branch} is back in ${tilde(b.worktree)}`);
          load();
        },
        (err) => setError(said(err)),
      )
      .finally(() => setBusy(null));
  };
  /** Gone for good, on the operator's yes: exactly the entries shown, so one
   *  kept after the card last looked is not touched. */
  const empty = async (gone: TrashEntry[], key: string, question: string) => {
    if (!(await ask(question))) return;
    setBusy({ what: "empty", key });
    setError("");
    setBack("");
    setRefused("");
    trashEmpty({ entries: gone.map(({ repo, name, stamp }) => ({ repo, name, stamp })), confirm: true })
      .then(
        (r) => {
          setRefused(r.refused.join(" · "));
          load();
        },
        (err) => setError(said(err)),
      )
      .finally(() => setBusy(null));
  };
  const emptyingAll = busy?.what === "empty" && busy.key === "all";
  const n = entries.length;
  const everything = `Empty the Trash? ${n === 1 ? "Its entry" : `All ${n} entries`} and the unsaved work in ${n === 1 ? "it" : "them"} go for good.`;
  return (
    <Card
      action={
        entries.length > 0 && (
          <button
            className={EMPTY}
            data-testid="trash-empty-all"
            disabled={!!busy}
            onClick={() => void empty(entries, "all", everything)}
            type="button"
          >
            <Trash2Icon className="size-3" />
            {emptyingAll ? "Emptying…" : "Empty"}
          </button>
        )
      }
      count={error ? undefined : entries.length}
      hue="var(--warn)"
      icon={<ArchiveIcon className="size-3.5" />}
      title="Trash"
    >
      {entries.map((e) => {
        const at = keptAt(e.stamp);
        const key = keyOf(e);
        return (
          <div className="flex min-w-0 flex-col gap-1 rounded-md border border-(--line-soft) bg-(--bg-app) px-2.5 py-2" data-testid="trash-entry" key={key}>
            <div className="flex min-w-0 items-center gap-2">
              <span className="min-w-0 flex-1 truncate font-bold font-mono text-(--fg) text-[11px]" title={`${e.name} in ${e.repo}`}>
                {e.name}
              </span>
              <button className={PLAIN} disabled={!!busy} onClick={() => putBack(e)} type="button">
                <ArchiveRestoreIcon className="size-3" />
                {busy?.what === "back" && busy.key === key ? "Putting back…" : "Put back"}
              </button>
              <button
                className={EMPTY}
                data-testid="trash-empty"
                disabled={!!busy}
                onClick={() => void empty([e], key, `Empty ${e.name} from the Trash? Its unsaved work goes for good.`)}
                type="button"
              >
                <Trash2Icon className="size-3" />
                {busy?.what === "empty" && busy.key === key ? "Emptying…" : "Empty"}
              </button>
            </div>
            <span className="truncate text-(--fg-dim) text-[10.5px]" title={e.repo}>
              {[tilde(e.repo), at?.toLocaleString(), e.commits > 0 && `${e.commits} ${e.commits === 1 ? "commit" : "commits"}`]
                .filter(Boolean)
                .join(" · ")}
            </span>
            <Changes changes={e.changes} />
          </div>
        );
      })}
      {back && <p className="text-(--ok) text-[11px]">{back}</p>}
      {(error || refused) && <p className="text-(--bad) text-[11px]">{error || refused}</p>}
    </Card>
  );
}
