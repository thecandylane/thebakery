// A delete, the operator's alone, exactly as the node's plan shows it: what
// goes (everything the deleted thing made, its unsaved work kept in git
// first) and what stays because something staying uses it. Their
// own project folder goes to the Trash only when ticked here (a new plan).
// Delete carries out the plan on screen, by its digest; one that changed
// since comes back to look at again. The main Baker is never deleted: its
// plan says so.

import { type ReactNode, useEffect, useState } from "react";
import {
  ArchiveIcon,
  BoxIcon,
  CircleAlertIcon,
  DatabaseIcon,
  FolderIcon,
  FolderXIcon,
  GitBranchIcon,
  HardDriveIcon,
  LinkIcon,
  MessageSquareIcon,
  NetworkIcon,
  PowerIcon,
  ShieldCheckIcon,
  Trash2Icon,
  XIcon,
} from "lucide-react";
import {
  type DeleteBranch,
  deleteConfirm,
  type DeleteDone,
  deletePlan,
  type DeletePlan,
  type DeleteSaved,
  type DeleteTarget,
  type DeleteWorktree,
} from "$lib/api.generated";
import { Chip, said } from "./ui";

const ENGINE: Record<string, string> = { apple: "Apple container", docker: "Docker", orbstack: "OrbStack" };
const tilde = (p: string) => p.replace(/^\/(Users|home)\/[^/]+(?=\/|$)/, "~");
const count = (n: number, one: string) => `${n} ${one}${n === 1 ? "" : "s"}`;
/** What the delete names, as the node does. */
const idOf = (t: DeleteTarget) => (t.kind === "project" ? t.project_id : t.kind === "repo" ? `${t.project_id} ${t.repo}` : t.stream_id);
const BUTTON =
  "flex h-7 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-3 text-(--fg-body) text-xs hover:text-(--fg) disabled:opacity-40";

/** One list of the plan: its name and count, then every row. */
function Group({ label, icon, children }: { label: string; icon: ReactNode; children: ReactNode[] }) {
  if (children.length === 0) return null;
  return (
    <section aria-label={label} className="flex flex-col">
      <div className="flex h-6 items-center gap-1.5 px-1 font-bold text-(--fg-dim) text-[10px] tracking-[.08em]">
        {icon}
        {label}
        <Chip>{children.length}</Chip>
      </div>
      {children}
    </section>
  );
}

function Row({ main, sub, mono = true, title }: { main: ReactNode; sub?: ReactNode; mono?: boolean; title?: string }) {
  return (
    <div className="flex min-h-6 min-w-0 items-center gap-2 rounded px-1.5 hover:bg-(--bg-chip)" title={title}>
      <span className={`min-w-0 flex-1 truncate text-(--fg-body) ${mono ? "font-mono text-[11px]" : "text-xs"}`}>{main}</span>
      {sub && <span className="flex min-w-0 max-w-[55%] shrink-0 items-center gap-1 truncate text-(--fg-dim) text-[10.5px]">{sub}</span>}
    </div>
  );
}

/** A half of the plan, in its hue: what goes, or what stays. */
function Side({ label, hue, icon, children }: { label: string; hue: string; icon: ReactNode; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col rounded-lg border border-(--line) bg-(--bg-raised)">
      <div
        className="flex h-8 items-center gap-2 rounded-t-lg border-(--line-soft) border-b px-3 font-bold text-[10.5px] tracking-[.08em]"
        style={{ background: `color-mix(in srgb, ${hue} 9%, transparent)`, boxShadow: `inset 3px 0 0 ${hue}`, color: hue }}
      >
        {icon}
        {label}
      </div>
      <div className="flex min-w-0 flex-col gap-1.5 p-2">{children}</div>
    </div>
  );
}

const engineThing = (t: { engine: string; name: string; by: string }) => (
  <Row key={`${t.engine}:${t.name}`} main={t.name} sub={`${ENGINE[t.engine] ?? t.engine} · ${t.by}`} title={`${t.name} · ${t.by}`} />
);
const folder = (p: string) => <Row key={p} main={tilde(p)} title={p} />;
const branch = (b: DeleteBranch) => <Row key={`${b.repo}:${b.name}`} main={b.name} sub={tilde(b.repo)} title={`${b.name} in ${b.repo}`} />;

/** Uncommitted changes, as git lists them. */
export function Changes({ changes }: { changes: { code: string; path: string }[] }) {
  if (changes.length === 0) return null;
  return (
    <div className="ml-1.5 max-h-28 overflow-auto rounded border border-(--line-soft) bg-(--bg-app) px-1.5 py-1 font-mono text-[10.5px]">
      {changes.map((c) => (
        <div className="flex gap-2" key={`${c.code}${c.path}`}>
          <span className="w-5 shrink-0 whitespace-pre text-(--warn)">{c.code}</span>
          <span className="min-w-0 truncate text-(--fg-body)">{c.path}</span>
        </div>
      ))}
    </div>
  );
}

/** Unsaved work kept in git before its worktree or branch goes. */
const saved = (k: DeleteSaved) => (
  <div className="flex min-w-0 flex-col gap-0.5 py-0.5" key={`${k.repo}:${k.name}`}>
    <Row
      main={k.name}
      sub={[k.commits > 0 && count(k.commits, "commit"), k.changes.length > 0 && count(k.changes.length, "change"), tilde(k.repo)].filter(Boolean).join(" · ")}
      title={`${k.name} in ${k.repo}`}
    />
    <Changes changes={k.changes} />
  </div>
);

/** A folder going to the Trash with its changes, or changes that can't be read. */
const trashChanges = (w: DeleteWorktree) => (
  <div className="flex min-w-0 flex-col gap-0.5 py-0.5" key={w.path}>
    <Row main={tilde(w.path)} sub={w.error ? "its changes can't be read" : count(w.changes.length, "change")} title={w.error ?? w.path} />
    <Changes changes={w.changes} />
  </div>
);

/** The operator's own project folders: to the Trash too, only when ticked. */
function Own({ plan, on, busy, flip }: { plan: DeletePlan; on: boolean; busy: boolean; flip: () => void }) {
  return (
    <label
      className={`flex min-w-0 cursor-pointer items-start gap-2 rounded-md border px-2.5 py-2 ${on ? "border-(--bad-border) bg-(--bad-bg)" : "border-(--line) bg-(--bg-raised)"}`}
      data-testid="delete-own"
    >
      <input checked={on} className="mt-0.5 accent-(--bad)" disabled={busy} onChange={flip} type="checkbox" />
      <span className="flex min-w-0 flex-1 flex-col gap-0.5">
        <span className="text-(--fg) text-xs">Also send the folder to the Trash</span>
        {plan.own.map((p) => (
          <span className="truncate font-mono text-(--fg-dim) text-[10.5px]" key={p} title={p}>
            {tilde(p)}
          </span>
        ))}
      </span>
      <Chip tone={on ? "bad" : "ok"}>{on ? "TO THE TRASH" : "STAYS"}</Chip>
    </label>
  );
}

/** What the plan says, laid out: what goes, and what stays. */
function PlanView({ plan, busy, trashOwn, flipOwn }: { plan: DeletePlan; busy: boolean; trashOwn: boolean; flipOwn: () => void }) {
  const goes = [
    plan.threads,
    plan.worktrees,
    plan.branches,
    plan.rooms,
    plan.builds,
    plan.containers,
    plan.volumes,
    plan.networks,
    plan.engines,
    plan.trash,
    plan.roots,
  ].some((list) => list.length > 0);
  const stays = plan.kept.length + plan.kept_branches.length > 0;
  const trashedWhole = plan.trash.filter((p) => !plan.trash_changes.some((w) => w.path === p));
  return (
    <>
      {plan.refused && (
        <p className="flex items-start gap-2 rounded-md border border-(--bad-border) bg-(--bad-bg) px-3 py-2 text-(--bad) text-xs" data-testid="delete-refused">
          <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
          {plan.refused}
        </p>
      )}
      {plan.own.length > 0 && <Own busy={busy} flip={flipOwn} on={trashOwn} plan={plan} />}
      <div className="grid min-w-0 grid-cols-2 items-start gap-3">
        <Side hue="var(--bad)" icon={<Trash2Icon className="size-3.5" />} label="GOES">
          {!goes && <p className="px-1 text-(--fg-faint)">nothing</p>}
          <Group icon={<MessageSquareIcon className="size-3" />} label="THREADS AND CHATS">
            {plan.threads.map((t) => (
              <div className="flex min-w-0 flex-col rounded px-1.5 py-0.5 hover:bg-(--bg-chip)" key={t.stream_id}>
                <span className="flex min-w-0 items-center gap-2">
                  <span className="min-w-0 flex-1 truncate text-(--fg-body) text-xs">{t.title}</span>
                  <Chip tone={t.lifetime === "living" ? "warn" : "neutral"}>{t.lifetime}</Chip>
                </span>
                <span className="truncate font-mono text-(--fg-faint) text-[10px]" title={t.folder || t.stream_id}>
                  {t.stream_id}
                  {t.folder && ` · ${tilde(t.folder)}`}
                </span>
              </div>
            ))}
          </Group>
          <Group icon={<GitBranchIcon className="size-3" />} label="WORKTREES">
            {plan.worktrees.map((w) => (
              <Row
                key={w.path}
                main={tilde(w.path)}
                sub={[w.branch, w.changes.length > 0 && count(w.changes.length, "change")].filter(Boolean).join(" · ")}
                title={w.path}
              />
            ))}
          </Group>
          <Group icon={<GitBranchIcon className="size-3" />} label="BRANCHES">
            {plan.branches.map(branch)}
          </Group>
          <Group icon={<ArchiveIcon className="size-3" />} label="UNSAVED WORK, KEPT IN GIT">
            {plan.saved.map(saved)}
          </Group>
          <Group icon={<HardDriveIcon className="size-3" />} label="ROOMS">
            {plan.rooms.map(folder)}
          </Group>
          <Group icon={<HardDriveIcon className="size-3" />} label="BUILD OUTPUT, DELETED OUTRIGHT">
            {plan.builds.map(folder)}
          </Group>
          <Group icon={<BoxIcon className="size-3" />} label="CONTAINERS">
            {plan.containers.map(engineThing)}
          </Group>
          <Group icon={<DatabaseIcon className="size-3" />} label="VOLUMES">
            {plan.volumes.map(engineThing)}
          </Group>
          <Group icon={<NetworkIcon className="size-3" />} label="NETWORKS">
            {plan.networks.map(engineThing)}
          </Group>
          <Group icon={<PowerIcon className="size-3" />} label="ENGINES STARTED TO CLEAR, THEN STOPPED">
            {plan.engines.map((e) => (
              <Row key={e} main={ENGINE[e] ?? e} mono={false} sub="off now" />
            ))}
          </Group>
          <Group icon={<FolderXIcon className="size-3" />} label="TO THE TRASH">
            {[...trashedWhole.map(folder), ...plan.trash_changes.map(trashChanges)]}
          </Group>
          <Group icon={<GitBranchIcon className="size-3" />} label="BRANCHES TO THE TRASH">
            {plan.trash_branches.map(branch)}
          </Group>
          <Group icon={<LinkIcon className="size-3" />} label="ROOTS LET GO">
            {plan.roots.map(folder)}
          </Group>
        </Side>
        <Side hue="var(--ok)" icon={<ShieldCheckIcon className="size-3.5" />} label="STAYS">
          {!stays && <p className="px-1 text-(--fg-faint)">nothing</p>}
          <Group icon={<FolderIcon className="size-3" />} label="FOLDERS">
            {plan.kept.map((k) => (
              <Row key={k.path} main={tilde(k.path)} sub={k.why} title={`${k.path} · ${k.why}`} />
            ))}
          </Group>
          <Group icon={<GitBranchIcon className="size-3" />} label="BRANCHES">
            {plan.kept_branches.map((b) => (
              <Row key={`${b.repo}:${b.name}`} main={b.name} sub={b.why} title={`${b.name} in ${b.repo} · ${b.why}`} />
            ))}
          </Group>
        </Side>
      </div>
    </>
  );
}

/** What a delete did, by the plan's own names, and each step that didn't go. */
function DoneView({ done }: { done: DeleteDone }) {
  const r = done.removed;
  const title = (id: string) => done.plan.threads.find((t) => t.stream_id === id)?.title ?? id;
  return (
    <Side hue={done.errors.length ? "var(--warn)" : "var(--ok)"} icon={<Trash2Icon className="size-3.5" />} label="WENT">
      {done.errors.length > 0 && (
        <section aria-label="Didn't go" className="flex flex-col gap-0.5">
          {done.errors.map((e) => (
            <p className="flex items-start gap-1.5 rounded px-1.5 py-0.5 font-mono text-(--bad) text-[11px]" key={e}>
              <CircleAlertIcon className="mt-px size-3 shrink-0" />
              {e}
            </p>
          ))}
        </section>
      )}
      <Group icon={<MessageSquareIcon className="size-3" />} label="THREADS AND CHATS">
        {r.threads.map((id) => (
          <Row key={id} main={title(id)} mono={false} sub={<span className="truncate font-mono">{id}</span>} />
        ))}
      </Group>
      <Group icon={<GitBranchIcon className="size-3" />} label="WORKTREES">
        {r.worktrees.map(folder)}
      </Group>
      <Group icon={<GitBranchIcon className="size-3" />} label="BRANCHES">
        {r.branches.map(branch)}
      </Group>
      <Group icon={<HardDriveIcon className="size-3" />} label="ROOMS">
        {r.rooms.map(folder)}
      </Group>
      <Group icon={<HardDriveIcon className="size-3" />} label="BUILD OUTPUT, DELETED">
        {r.builds.map(folder)}
      </Group>
      <Group icon={<BoxIcon className="size-3" />} label="CONTAINERS">
        {r.containers.map((n) => <Row key={n} main={n} />)}
      </Group>
      <Group icon={<DatabaseIcon className="size-3" />} label="VOLUMES">
        {r.volumes.map((n) => <Row key={n} main={n} />)}
      </Group>
      <Group icon={<NetworkIcon className="size-3" />} label="NETWORKS">
        {r.networks.map((n) => <Row key={n} main={n} />)}
      </Group>
      <Group icon={<FolderXIcon className="size-3" />} label="TO THE TRASH">
        {r.trash.map(folder)}
      </Group>
      <Group icon={<LinkIcon className="size-3" />} label="ROOTS LET GO">
        {r.roots.map(folder)}
      </Group>
    </Side>
  );
}

export function DeleteDialog({ target, onClose, onDone }: { target: DeleteTarget; onClose: () => void; onDone: (done: DeleteDone) => void }) {
  /** The operator's own project folder goes to the Trash too. */
  const [trashOwn, setTrashOwn] = useState(false);
  /** Bumped to ask for the plan again. */
  const [look, setLook] = useState(0);
  /** The last plan the node gave, kept on screen while the next one comes. */
  const [plan, setPlan] = useState<DeletePlan | null>(null);
  const [planning, setPlanning] = useState(true);
  const [planError, setPlanError] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState<DeleteDone | null>(null);
  useEffect(() => {
    let live = true;
    setPlanning(true);
    deletePlan({ target, trash_own: trashOwn })
      .then(
        (p) => {
          if (!live) return;
          setPlan(p);
          setPlanError("");
        },
        (e) => live && setPlanError(said(e)),
      )
      .finally(() => live && setPlanning(false));
    return () => {
      live = false;
    };
  }, [target, trashOwn, look]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && !busy && onClose();
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  const flipOwn = () => {
    setError("");
    setTrashOwn((on) => !on);
  };
  /** A fresh plan: a turn finished. After a look that failed, from the
   *  start: the folder stays. */
  const again = () => {
    setError("");
    if (planError) setTrashOwn(false);
    setLook((n) => n + 1);
  };
  const confirm = () => {
    if (!plan) return;
    setBusy(true);
    setError("");
    deleteConfirm({ target, trash_own: trashOwn, digest: plan.digest })
      .then(
        (d) => {
          setDone(d);
          onDone(d);
          // Gone cleanly, it closes by itself; what couldn't go stays to be read.
          if (d.errors.length === 0) onClose();
        },
        (e) => {
          // A plan that changed since (or a refusal): what is there now shows.
          setError(said(e));
          setLook((n) => n + 1);
        },
      )
      .finally(() => setBusy(false));
  };
  const title = done?.plan.title ?? plan?.title ?? "";
  const shownError = error || planError;
  const ready = !!plan && !planning && !planError && !plan.refused && !busy;

  return (
    <div className="fixed inset-0 z-50 grid place-items-center bg-black/45 p-6" onPointerDown={(e) => e.target === e.currentTarget && !busy && onClose()}>
      <div
        aria-label={`Delete ${title}`}
        aria-modal="true"
        className="flex max-h-[86vh] w-[min(900px,94vw)] flex-col rounded-[10px] border border-(--line-strong) bg-(--bg-panel) text-xs shadow-(--shadow-menu)"
        data-testid="delete-dialog"
        role="dialog"
      >
        <header className="flex items-center gap-3 border-(--line-soft) border-b px-5 py-3.5">
          <span className="grid size-8 shrink-0 place-items-center rounded-[7px] border border-(--bad-border) bg-(--bad-bg) text-(--bad)">
            <Trash2Icon className="size-4" />
          </span>
          <div className="flex min-w-0 flex-1 flex-col gap-0.5">
            <span className="flex min-w-0 items-center gap-2">
              <h2 className="m-0 min-w-0 truncate font-bold text-(--fg) text-[15px]">
                {done ? "Deleted" : "Delete"} {title || "…"}
              </h2>
              <Chip tone="bad">{target.kind.toUpperCase()}</Chip>
            </span>
            <span className="truncate font-mono text-(--fg-faint) text-[10.5px]" title={idOf(target)}>
              {idOf(target)}
            </span>
          </div>
          <button
            aria-label="Close"
            className="grid size-7 place-items-center rounded text-(--fg-dim) hover:bg-(--bg-chip) hover:text-(--fg) disabled:opacity-40"
            disabled={busy}
            onClick={onClose}
            type="button"
          >
            <XIcon className="size-4" />
          </button>
        </header>

        <div className={`flex min-h-0 flex-col gap-3 overflow-auto px-5 py-4 ${planning && plan && !done ? "opacity-60" : ""}`}>
          {done ? (
            <DoneView done={done} />
          ) : plan ? (
            <PlanView busy={busy || planning} flipOwn={flipOwn} plan={plan} trashOwn={trashOwn} />
          ) : (
            planning && (
              <p className="flex items-center gap-2 text-(--fg-faint)">
                <span className="size-3 animate-spin rounded-full border-2 border-(--line) border-t-(--bad)" />
                Reading what it takes…
              </p>
            )
          )}
        </div>

        <footer className="flex items-center gap-2 border-(--line-soft) border-t px-5 py-3">
          {shownError ? (
            <span className="min-w-0 flex-1 truncate text-(--bad) text-[11px]" title={shownError}>
              {shownError}
            </span>
          ) : (
            <span className="min-w-0 flex-1 truncate font-mono text-(--fg-faint) text-[10.5px]">
              {done ? `delete.done · ${idOf(target)}` : plan ? `digest ${plan.digest}` : ""}
            </span>
          )}
          {done ? (
            <button className={BUTTON} onClick={onClose} type="button">
              Close
            </button>
          ) : (
            <>
              <button className={BUTTON} disabled={planning || busy} onClick={again} type="button">
                Look again
              </button>
              <button className={BUTTON} disabled={busy} onClick={onClose} type="button">
                Cancel
              </button>
              <button
                className="flex h-7 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--bad-border) bg-(--bad) px-3 font-bold text-(--fg-on-solid) text-xs hover:brightness-110 disabled:opacity-40"
                data-testid="delete-confirm"
                disabled={!ready}
                onClick={confirm}
                type="button"
              >
                <Trash2Icon className="size-3.5" />
                {busy ? "Deleting…" : `Delete ${target.kind}`}
              </button>
            </>
          )}
        </footer>
      </div>
    </div>
  );
}
