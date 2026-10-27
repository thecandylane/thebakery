// A thread's own menu, a project's removal, and the Delete in every row's
// menu (the operator's QA: clear old threads and projects). The classic
// thread menu's actions in its words (conversationActions): rename, archive
// or restore, make a chat a project, delete; plus adding a thread to a
// project or moving it to another. Each runs straight on the node; Remove is
// asked first, and Delete (a thread's, a project's, a repo's, a
// workspace's) opens its plan, carried out only on the operator's click.

import { createContext, type ReactNode, useContext, useEffect, useLayoutEffect, useRef, useState } from "react";
import { FolderIcon, MoreHorizontalIcon, Trash2Icon } from "lucide-react";
import { chatMakeProject, conversationArchive, conversationRename, type DeleteTarget, projectRetire, projectSocketAttach } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { askText } from "$lib/askText";
import { type Conversation, conversationActions } from "$lib/conversations";
import { pickFolder } from "$lib/desktop";
import * as L from "./layout";
import { useDismiss } from "./PanelMenu";
import type { ShellProject, ShellSnapshot } from "./shellSource.svelte";
import { said } from "./ui";

export type ThreadActions = {
  snap: ShellSnapshot;
  refresh: () => void;
  setLayout: (change: (l: L.Layout) => L.Layout) => void;
  /** Opens a delete's plan (DeleteDialog). */
  remove: (target: DeleteTarget) => void;
};
/** What the menus need from the shell, provided once at its root. */
export const ThreadMenuContext = createContext<ThreadActions | null>(null);

/** The actions this menu runs, of the classic list (the rest open pages or
 *  stop turns, which the panel does itself). */
const HERE = new Set(["rename", "archive", "restore", "project", "project-folder", "delete"]);
const ROW = "flex min-h-7 w-full items-center gap-2 rounded-[5px] px-2 text-left hover:bg-(--bg-chip) disabled:opacity-40 disabled:hover:bg-transparent";
const LABEL = "px-2 pt-1.5 pb-0.5 font-bold text-(--fg-dim) text-[9.5px] tracking-[.08em]";

/** A row's More button and its menu, which sits in the window under the
 *  button and inward, flipped to whichever side keeps it whole (a sidebar
 *  row's ran off the left). `className` holds while it's closed, so a row
 *  can show it only on hover; open, it always shows. */
function RowMenu({ label, open, setOpen, className, children }: { label: string; open: boolean; setOpen: (open: boolean) => void; className: string; children: ReactNode }) {
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const [at, setAt] = useState<{ left: number; top: number } | null>(null);
  useLayoutEffect(() => {
    if (!open) return setAt(null);
    const b = trigger.current?.getBoundingClientRect();
    const m = menu.current?.getBoundingClientRect();
    if (!b || !m) return;
    const pad = 8;
    const fit = (x: number, size: number, room: number) => Math.max(pad, Math.min(x, room - size - pad));
    const left = b.right - m.width >= pad ? b.right - m.width : b.left;
    const top = b.bottom + 4 + m.height <= window.innerHeight - pad ? b.bottom + 4 : b.top - 4 - m.height;
    setAt({ left: fit(left, m.width, window.innerWidth), top: fit(top, m.height, window.innerHeight) });
  }, [open]);
  // It stays put in the window, so a scroll or a resize under it closes it.
  useEffect(() => {
    if (!open) return;
    const away = () => setOpen(false);
    window.addEventListener("scroll", away, true);
    window.addEventListener("resize", away);
    return () => {
      window.removeEventListener("scroll", away, true);
      window.removeEventListener("resize", away);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);
  return (
    <div className={`relative ${open ? "" : className}`} ref={box}>
      <button
        aria-expanded={open}
        aria-label={`More for ${label}`}
        className="grid size-5 shrink-0 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg)"
        ref={trigger}
        onClick={(e) => {
          e.stopPropagation();
          setOpen(!open);
        }}
        title="More"
        type="button"
      >
        <MoreHorizontalIcon className="size-3.5" />
      </button>
      {open && (
        <div
          aria-label={`${label}: actions`}
          className="fixed z-50 w-[250px] rounded-md border border-(--line-strong) bg-(--bg-raised) p-1 text-(--fg) text-xs shadow-(--shadow-menu)"
          ref={menu}
          role="menu"
          style={at ? { left: at.left, top: at.top } : { left: 0, top: 0, visibility: "hidden" }}
        >
          {children}
        </div>
      )}
    </div>
  );
}

/** The menu of a project's, a repo's or a workspace's row: its Delete. */
export function DeleteMenu({ target, label, className = "" }: { target: DeleteTarget; label: string; className?: string }) {
  const shell = useContext(ThreadMenuContext);
  const [open, setOpen] = useState(false);
  if (!shell) return null;
  return (
    <RowMenu className={className} label={label} open={open} setOpen={setOpen}>
      <button
        className={`${ROW} text-(--bad)`}
        onClick={() => {
          setOpen(false);
          shell.remove(target);
        }}
        role="menuitem"
        type="button"
      >
        <Trash2Icon className="size-3 shrink-0" />
        Delete {target.kind}…
      </button>
    </RowMenu>
  );
}

export function ThreadMenu({ streamId, className = "", onOpenChange }: { streamId: string; className?: string; onOpenChange?: (open: boolean) => void }) {
  const shell = useContext(ThreadMenuContext);
  const [open, setOpenState] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const setOpen = (next: boolean) => {
    setOpenState(next);
    if (!next) setError("");
    onOpenChange?.(next);
  };
  const c = shell && (shell.snap.conversations.find((x) => x.streamId === streamId) ?? shell.snap.archived.find((x) => x.streamId === streamId));
  if (!shell || !c) return null;
  const actions = conversationActions(c, "", busy).filter((a) => HERE.has(a.id));
  const home = shell.snap.projects.find((p) => p.streamIds.includes(c.streamId));
  // A Baker thread, the main one and a task stay where they are.
  // An orc and its workers belong to their workspace on the Baker's side.
  const inWorkspace = c.workspace || shell.snap.conversations.some((o) => o.workspace && o.streamId === c.openedBy);
  const targets = c.bakerThread || c.main || c.kind === "task" || inWorkspace ? [] : shell.snap.projects.filter((p) => p !== home);
  if (actions.length === 0 && targets.length === 0) return null;

  /** Runs one; a `false` from it is the operator saying no. */
  const go = async (work: () => Promise<boolean>) => {
    setBusy(true);
    setError("");
    try {
      if (await work()) {
        setOpen(false);
        shell.refresh();
      }
    } catch (e) {
      setError(said(e));
    } finally {
      setBusy(false);
    }
  };
  const run = (id: string) => go(() => act(id, c));
  const attach = (p: ShellProject) => go(() => projectSocketAttach({ stream_id: c.streamId, project_id: p.id }).then(() => true));
  const del = actions.find((a) => a.id === "delete");

  return (
    <RowMenu className={className} label={c.title} open={open} setOpen={setOpen}>
      {actions
        .filter((a) => a !== del)
        .map((a) => (
          <button className={ROW} disabled={a.disabled || busy} key={a.id} onClick={() => void run(a.id)} role="menuitem" title={a.hint} type="button">
            {a.label}
          </button>
        ))}
      {targets.length > 0 && (
        <>
          <div className={LABEL}>{home ? "MOVE TO PROJECT" : "ADD TO PROJECT"}</div>
          {targets.map((p) => (
            <button className={ROW} disabled={busy || c.busy} key={p.id} onClick={() => void attach(p)} role="menuitem" type="button">
              <FolderIcon className="size-3 shrink-0 text-(--hue-project)" />
              <span className="min-w-0 flex-1 truncate">{p.name}</span>
            </button>
          ))}
        </>
      )}
      {del && (
        <>
          <div className="mx-1 my-1 h-px bg-(--line)" />
          <button
            className={`${ROW} text-(--bad)`}
            disabled={del.disabled || busy}
            onClick={() => {
              setOpen(false);
              shell.remove({ kind: "thread", stream_id: c.streamId });
            }}
            role="menuitem"
            title={del.hint}
            type="button"
          >
            <Trash2Icon className="size-3 shrink-0" />
            {del.label}
          </button>
        </>
      )}
      {error && <p className="px-2 py-1 text-(--bad) text-[11px]">{error}</p>}
    </RowMenu>
  );
}

/** One of the classic menu's actions on `c`; false when the operator says no. */
async function act(id: string, c: Conversation): Promise<boolean> {
  switch (id) {
    case "rename": {
      const title = await askText("Rename thread", c.title);
      if (!title || title === c.title) return false;
      await conversationRename({ stream_id: c.streamId, title });
      return true;
    }
    case "archive":
    case "restore":
      await conversationArchive({ stream_id: c.streamId, archived: id === "archive" });
      return true;
    case "project":
    case "project-folder": {
      const parent = id === "project-folder" ? await pickFolder(`Where "${c.title}" goes`) : null;
      if (id === "project-folder" && !parent) return false;
      const name = await askText("Project name", c.title);
      if (!name) return false;
      const where = parent ?? "Bakery's projects folder";
      if (!(await ask(`Make "${name}" a project?\n\nIts files move to ${where} as a Git repository with a first commit, and the thread carries on there. Its runtime restarts.`))) return false;
      await chatMakeProject({ stream_id: c.streamId, parent, name });
      return true;
    }
  }
  return false;
}

/** A project's Remove, for its control plane's header. */
export function RemoveProjectButton({ project, className }: { project: ShellProject; className: string }) {
  const shell = useContext(ThreadMenuContext);
  const [error, setError] = useState("");
  if (!shell) return null;
  return (
    <>
      {error && (
        <span className="max-w-48 truncate text-(--bad) text-[11px]" title={error}>
          {error}
        </span>
      )}
      <button
        className={className}
        onClick={() => {
          setError("");
          removeProject(project, shell).catch((e) => setError(said(e)));
        }}
        title={`Remove ${project.name} from Bakery`}
        type="button"
      >
        <Trash2Icon className="size-3 text-(--bad)" />
        Remove
      </button>
    </>
  );
}

/** Removes a project from Bakery (the node hides it: its files, roots and
 *  history are kept), asked first; its workspace goes from the layout. */
export async function removeProject(p: ShellProject, shell: ThreadActions): Promise<boolean> {
  if (!(await ask(`Remove "${p.name}" from Bakery?\n\nThis hides it here; its files, roots, and session history are kept. Its threads move to Ungrouped.`))) return false;
  await projectRetire({ project_id: p.id });
  shell.setLayout((l) => L.dropProject(l, p.id));
  shell.refresh();
  return true;
}
