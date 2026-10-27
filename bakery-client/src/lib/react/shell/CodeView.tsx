// A thread's Code view (Round 4, C1): the Bakery's own, review first. Its
// worktree's files, the ones it changed against the branch it started from
// first; each file read with its changes marked in the gutter, or as its
// changes block by block (who wrote each, and Ask). "Open in" hands the
// file to the Code editor (code-server, Bakery-styled) or to an editor on
// this Mac, the last one used first.

import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { ChevronDownIcon, ChevronRightIcon, CodeIcon, FileIcon, FolderIcon, GitBranchIcon } from "lucide-react";
import { type DirEntryInfo, filesList, filesRead, gitDiff, gitProvenance, gitStatus, type HunkSource } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { electronShell } from "$lib/desktop";
import { languageForPath, tokenStyle, useHighlighted } from "../highlight";
import { byFolder, type CodeEntry, codeEntries, diskHunks, gutterOf, inside, type Mark } from "./codeModel";
import { folderStream, membersOf, worktreeGroups } from "./floorModel";
import * as L from "./layout";
import { type Candidate, HunkBlock, Provenance } from "./MergeReview";
import { POP, useDismiss } from "./PanelMenu";
import { baseOf } from "./reviewModel";
import { useRollup } from "./rollup";
import type { ShellCheckout, ShellProject } from "./shellSource.svelte";
import { Chip, said, type ToolContext, useSecretShown } from "./ui";
import { type Hunk, parseUnifiedDiff } from "./unifiedDiff";

const BUTTON =
  "flex h-6 shrink-0 items-center gap-1.5 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2 text-(--fg-body) text-[11px] hover:text-(--fg) disabled:opacity-40";
const SEG = "h-[22px] px-2.5 text-[11px] disabled:opacity-40";
const SEG_ON = "bg-(--bg-pressed) font-bold text-(--accent-fg)";
const STATUS: Record<CodeEntry["status"], [string, string]> = {
  added: ["A", "var(--ok)"],
  modified: ["M", "var(--warn)"],
  deleted: ["D", "var(--bad)"],
  renamed: ["R", "var(--violet)"],
  binary: ["B", "var(--fg-dim)"],
};
const MARK: Record<Mark, string> = { add: "var(--diff-add-fg)", mod: "var(--warn)", del: "var(--diff-del-fg)" };
const ROW_TINT: Partial<Record<Mark, string>> = { add: "var(--diff-add-bg)", mod: "var(--warn-bg)" };
const basename = (p: string) => p.split("/").pop() || p;

/** Files the shell asked a Code view to show (an edit row's file, a search
 *  match), by thread: its view takes one at once, or when it next opens. */
const wanted = new Map<string, { path: string; line: number }>();
const wanting = new Set<() => void>();

export function revealInCode(streamId: string, path: string, line: number) {
  wanted.set(streamId, { path, line });
  for (const take of wanting) take();
}

export function CodeView({ ctx, streamId }: { ctx: ToolContext; streamId: string }) {
  // A Baker conversation's folder is the Bakery's: its own worktree, else the main checkout.
  const sid = folderStream(ctx.snap.conversations, [...ctx.snap.projects, ctx.snap.bakery].flatMap((p) => p.checkouts), streamId);
  const project = [...ctx.snap.projects, ctx.snap.bakery].find((p) => p.checkouts.some((c) => c.streamId === sid && c.path));
  const co = project?.checkouts.find((c) => c.streamId === sid);
  if (!project || !co) return <p className="p-4 text-(--fg-faint) text-xs">This thread has no folder</p>;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <WorktreeStrip ctx={ctx} kind="code" streamId={sid} />
      <div className="min-h-0 flex-1">
        <Code co={co} ctx={ctx} key={sid} project={project} streamId={sid} />
      </div>
    </div>
  );
}

/** The worktrees a workspace's one Code panel (and Preview, and Code
 *  editor) can look at: the main checkout first, then each worktree by
 *  branch; in an orc's workspace, the orc's first, then its workers'.
 *  Picking one turns the panel to it, through the first of its
 *  conversations. */
export function WorktreeStrip({ ctx, kind, streamId }: { ctx: ToolContext; kind: "code" | "preview" | "editor"; streamId: string }) {
  const { snap, workspace } = ctx;
  const baker = workspace.id === L.BAKER;
  const members = baker ? snap.conversations.filter((c) => c.bakerThread) : membersOf(workspace, snap.projects, snap.conversations);
  // An orc's workers may be the Bakery's own or a project's: every checkout is looked in.
  const checkouts = baker ? snap.bakery.checkouts : workspace.orc ? [...snap.projects, snap.bakery].flatMap((p) => p.checkouts) : snap.projects.flatMap((p) => p.checkouts);
  const all = worktreeGroups(members, checkouts).filter((g) => g.path);
  const orcs = (g: (typeof all)[number]) => g.members.some((m) => m.streamId === workspace.orc);
  const groups = workspace.orc ? [...all.filter(orcs), ...all.filter((g) => !orcs(g))] : all;
  if (groups.length < 2) return null;
  return (
    <div aria-label="Whose worktree" className="flex h-8 shrink-0 items-center gap-1 overflow-x-auto border-(--line-soft) border-b bg-(--bg-chrome) px-2" role="tablist">
      {groups.map((g) => {
        const on = g.members.some((m) => m.streamId === streamId);
        const Icon = g.local ? FolderIcon : GitBranchIcon;
        return (
          <button
            aria-selected={on}
            className={`${SEG} flex shrink-0 items-center gap-1.5 rounded-[5px] ${on ? SEG_ON : "text-(--fg-2) hover:bg-(--bg-chip)"}`}
            key={g.path}
            onClick={() => on || ctx.setLayout((l) => L.openPanel(l, workspace.id, `@${kind}:${g.members[0].streamId}`))}
            role="tab"
            title={g.members.map((m) => m.title).join(", ")}
            type="button"
          >
            <Icon className={`size-3 ${g.local ? "text-(--hue-project)" : "text-(--violet)"}`} />
            {g.branch || basename(g.path)}
          </button>
        );
      })}
    </div>
  );
}

function Code({ ctx, streamId, project, co }: { ctx: ToolContext; streamId: string; project: ShellProject; co: ShellCheckout }) {
  const conv = ctx.snap.conversations.find((c) => c.streamId === streamId) ?? null;
  const rollup = useRollup(project.id);
  // The branch it started from, as the node recorded it; else the project's target.
  const base = co.inPlace
    ? null
    : rollup.data?.worktrees.find((w) => w.stream_id === streamId)?.base || baseOf(project.checkouts)?.branch || null;
  const [entries, setEntries] = useState<CodeEntry[] | null>(null);
  const [cut, setCut] = useState(false);
  /** A diff shows lines of a secret file. */
  const [secret, setSecret] = useState(false);
  useSecretShown(secret);
  /** The open file, when the node says it's a secret. */
  const [secretFile, setSecretFile] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [sources, setSources] = useState<ReadonlyMap<string, HunkSource>>(new Map());
  const [tab, setTab] = useState<"changed" | "all">("changed");
  const [picked, setPicked] = useState<string | null>(null);
  const [mode, setMode] = useState<"read" | "changes">("read");
  const [target, setTarget] = useState<{ path: string; line: number; at: number } | null>(null);
  // Read again when the thread moves on: its status, or what it has changed.
  const stamp = `${conv?.status}:${co.added}:${co.removed}`;

  useEffect(() => {
    let live = true;
    const none = Promise.resolve({ diff: "", truncated: false, secret: false });
    Promise.all([
      base ? gitDiff({ stream_id: streamId, base }) : none,
      gitDiff({ stream_id: streamId, staged: true }),
      gitDiff({ stream_id: streamId }),
      gitStatus({ stream_id: streamId }),
      // The file on disk against the fork point: its gutter, committed and not alike.
      base ? gitDiff({ stream_id: streamId, base, worktree: true }) : none,
    ]).then(
      ([branch, staged, unstaged, status, disk]) => {
        if (!live) return;
        const loose = [...parseUnifiedDiff(staged.diff), ...parseUnifiedDiff(unstaged.diff)];
        // git lists a new folder as one entry; its files show under All files.
        const files = status.files.filter((f) => !f.path.endsWith("/"));
        setEntries(codeEntries(parseUnifiedDiff(branch.diff), loose, files, parseUnifiedDiff(disk.diff)));
        setCut(branch.truncated || staged.truncated || unstaged.truncated || disk.truncated);
        setSecret(branch.secret || staged.secret || unstaged.secret || disk.secret);
        setError("");
      },
      (e) => live && setError(said(e)),
    );
    // Who wrote each block of the branch: extra, the view works without it.
    if (base) {
      gitProvenance({ stream_id: streamId, base }).then(
        (p) => live && setSources(new Map(p.hunks.map((h) => [`${h.file}:${h.new_start}`, h]))),
        () => {},
      );
    }
    return () => {
      live = false;
    };
  }, [streamId, base, stamp]);

  // Opens on its first changed file, or on all files when it changed none.
  const first = entries?.[0]?.path ?? null;
  const loaded = entries !== null;
  useEffect(() => {
    if (!loaded) return;
    setPicked((p) => p ?? first);
    if (!first) setTab((t) => (t === "changed" ? "all" : t));
  }, [loaded, first]);

  const known = useRef<readonly CodeEntry[] | null>(null);
  known.current = entries;
  useEffect(() => {
    const take = () => {
      const w = wanted.get(streamId);
      if (!w) return;
      wanted.delete(streamId);
      const path = inside(co.path, w.path);
      setPicked(path);
      setMode("read");
      setTarget({ path, line: w.line, at: Date.now() });
      if (known.current && !known.current.some((e) => e.path === path)) setTab("all");
    };
    take();
    wanting.add(take);
    return () => {
      wanting.delete(take);
    };
  }, [streamId, co.path]);

  const entry = entries?.find((e) => e.path === picked) ?? null;
  const hunks = entry ? (entry.committed?.hunks.length ?? 0) + entry.uncommitted.reduce((n, f) => n + f.hunks.length, 0) : 0;
  const add = entry ? (entry.committed?.add ?? 0) + entry.uncommitted.reduce((n, f) => n + f.add, 0) : 0;
  const del = entry ? (entry.committed?.del ?? 0) + entry.uncommitted.reduce((n, f) => n + f.del, 0) : 0;
  const candidate: Candidate = { ...co, conv, base: base ?? co.branch };
  const dir = picked?.includes("/") ? picked.slice(0, picked.lastIndexOf("/") + 1) : "";

  return (
    <div className="flex h-full min-h-0 bg-(--bg-app) text-xs" data-testid="code-view">
      <aside aria-label="Files" className="flex w-[min(250px,36%)] shrink-0 flex-col border-(--line-soft) border-r bg-(--bg-chrome)">
        <div className="flex h-8 shrink-0 items-center gap-2 border-(--line-soft) border-b px-2.5" title={base ? `${co.branch} against ${base}` : co.branch}>
          <GitBranchIcon className="size-3 shrink-0 text-(--violet)" />
          <span className="min-w-0 truncate font-bold text-(--fg)">{co.branch || "detached"}</span>
          {base && <span className="min-w-0 shrink-[2] truncate text-(--fg-dim) text-[11px]">→ {base}</span>}
        </div>
        <div className="p-1.5">
          <div aria-label="Which files" className="flex overflow-hidden rounded-[5px] border border-(--line)" role="tablist">
            <button aria-selected={tab === "changed"} className={`${SEG} flex-1 ${tab === "changed" ? SEG_ON : "text-(--fg-2)"}`} onClick={() => setTab("changed")} role="tab" type="button">
              Changed · {entries?.length ?? "…"}
            </button>
            <button
              aria-selected={tab === "all"}
              className={`${SEG} flex-1 border-(--line) border-l ${tab === "all" ? SEG_ON : "text-(--fg-2)"}`}
              onClick={() => setTab("all")}
              role="tab"
              type="button"
            >
              All files
            </button>
          </div>
        </div>
        <div className="min-h-0 flex-1 overflow-auto pb-2">
          {error && <p className="px-2.5 text-(--warn)">{error}</p>}
          {tab === "changed" ? (
            <ChangedFiles entries={entries ?? []} onPick={setPicked} picked={picked} />
          ) : (
            <AllFiles changed={entries ?? []} dir="" onPick={setPicked} picked={picked} streamId={streamId} />
          )}
          {cut && tab === "changed" && <p className="px-2.5 pt-1 text-(--warn) text-[11px]">The diff was too large to read whole</p>}
        </div>
      </aside>

      <section className="flex min-w-0 flex-1 flex-col">
        {picked && (
          <div className="flex h-8 shrink-0 items-center gap-2 border-(--line-soft) border-b bg-(--bg-raised) px-3">
            <span className="min-w-0 truncate" title={picked}>
              <span className="text-(--fg-dim)">{dir}</span>
              <span className="font-bold text-(--fg)">{basename(picked)}</span>
            </span>
            {add + del > 0 && (
              <span className="shrink-0 font-mono text-[11px]">
                <span className="text-(--diff-add-fg)">+{add}</span> <span className="text-(--diff-del-fg)">−{del}</span>
              </span>
            )}
            {entry?.conflicted && <Chip tone="bad">CONFLICT</Chip>}
            {entry?.untracked && <Chip>NEW</Chip>}
            {secretFile === picked && <Chip tone="bad">SECRET</Chip>}
            <span className="flex-1" />
            <div className="flex overflow-hidden rounded-[5px] border border-(--line)" role="tablist">
              <button aria-selected={mode === "read"} className={`${SEG} ${mode === "read" ? SEG_ON : "text-(--fg-2)"}`} onClick={() => setMode("read")} role="tab" type="button">
                Read
              </button>
              <button
                aria-selected={mode === "changes"}
                className={`${SEG} border-(--line) border-l ${mode === "changes" ? SEG_ON : "text-(--fg-2)"}`}
                disabled={hunks === 0}
                onClick={() => setMode("changes")}
                role="tab"
                type="button"
              >
                Changes
              </button>
            </div>
            <OpenIn ctx={ctx} file={picked} folder={co.path} line={target?.path === picked ? target.line : 1} streamId={streamId} working={conv?.status === "working"} />
          </div>
        )}
        {picked && mode === "read" && (
          <ReadFile
            entry={entry}
            key={picked}
            onAsk={() => setMode("changes")}
            onRead={(isSecret) => setSecretFile(isSecret ? picked : null)}
            path={picked}
            sources={sources}
            stamp={stamp}
            streamId={streamId}
            target={target?.path === picked ? target : null}
          />
        )}
        {picked && mode === "changes" && entry && (
          <section aria-label={`Changes to ${picked}`} className="flex min-w-0 flex-1 flex-col gap-3 overflow-auto p-3">
            {entry.committed?.hunks.map((h, i) => (
              <HunkBlock
                candidate={candidate}
                ctx={ctx}
                file={entry.committed!}
                hunk={h}
                key={`c${i}:${h.header}`}
                project={project.name}
                secret={secret}
                source={sources.get(`${entry.path}:${h.newStart}`)}
              />
            ))}
            {entry.uncommitted.length > 0 && <p className="font-bold text-(--fg-dim) text-[10.5px] tracking-[.06em]">NOT COMMITTED</p>}
            {entry.uncommitted.flatMap((f, j) =>
              f.hunks.map((h, i) => <HunkBlock candidate={candidate} ctx={ctx} file={f} hunk={h} key={`u${j}:${i}:${h.header}`} project={project.name} secret={secret} />),
            )}
          </section>
        )}
      </section>
    </div>
  );
}

function FileRow({ name, status, on, depth, onClick, extra }: {
  name: string;
  status: CodeEntry["status"] | null;
  on: boolean;
  depth: number;
  onClick: () => void;
  extra?: ReactNode;
}) {
  const [letter, color] = status ? STATUS[status] : ["", "var(--fg-dim)"];
  return (
    <button
      className={`flex h-6 w-full min-w-0 items-center gap-1.5 pr-2 text-left ${on ? "bg-(--accent-sel) text-(--fg) shadow-[inset_0_0_0_1px_var(--accent-border)]" : "text-(--fg-body) hover:bg-(--bg-chip)"}`}
      onClick={onClick}
      style={{ paddingLeft: 10 + depth * 14 }}
      title={name}
      type="button"
    >
      <FileIcon className="size-3 shrink-0" style={{ color }} />
      <span className="min-w-0 flex-1 truncate">{name}</span>
      {extra}
      {letter && (
        <span className="shrink-0 font-bold text-[10px]" style={{ color }}>
          {letter}
        </span>
      )}
    </button>
  );
}

function ChangedFiles({ entries, picked, onPick }: { entries: readonly CodeEntry[]; picked: string | null; onPick: (path: string) => void }) {
  const byPath = new Map(entries.map((e) => [e.path, e]));
  return (
    <>
      {byFolder(entries.map((e) => e.path)).map(({ dir, files }) => (
        <div key={dir}>
          {dir && (
            <div className="flex h-6 items-center gap-1.5 px-2.5 text-(--fg-dim)" title={dir}>
              <FolderIcon className="size-3 shrink-0" />
              <span className="truncate">{dir}</span>
            </div>
          )}
          {files.map((name) => {
            const e = byPath.get(dir + name)!;
            return (
              <FileRow
                depth={dir ? 1 : 0}
                extra={
                  e.uncommitted.length > 0 || e.untracked ? (
                    <span className="shrink-0 text-(--warn)" title="Not committed">
                      •
                    </span>
                  ) : null
                }
                key={name}
                name={name}
                on={e.path === picked}
                onClick={() => onPick(e.path)}
                status={e.conflicted ? "deleted" : e.status}
              />
            );
          })}
        </div>
      ))}
    </>
  );
}

/** The worktree's folders, opened one at a time; changed files keep their letter. */
function AllFiles({ streamId, dir, changed, picked, onPick, depth = 0 }: {
  streamId: string;
  dir: string;
  changed: readonly CodeEntry[];
  picked: string | null;
  onPick: (path: string) => void;
  depth?: number;
}) {
  const [items, setItems] = useState<DirEntryInfo[] | null>(null);
  const [error, setError] = useState("");
  const [open, setOpen] = useState<ReadonlySet<string>>(new Set());
  useEffect(() => {
    let live = true;
    filesList({ stream_id: streamId, path: dir || null }).then(
      (list) => live && setItems([...list].sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name))),
      (e) => live && setError(said(e)),
    );
    return () => {
      live = false;
    };
  }, [streamId, dir]);
  const status = new Map(changed.map((e) => [e.path, e.status]));
  if (error) return <p className="px-2.5 text-(--warn)">{error}</p>;
  if (!items) return <p className="px-2.5 text-(--fg-faint)">…</p>;
  return (
    <>
      {items.map((it) => {
        const path = `${dir}${it.name}`;
        if (!it.is_dir) {
          return <FileRow depth={depth} key={it.name} name={it.name} on={path === picked} onClick={() => onPick(path)} status={status.get(path) ?? null} />;
        }
        const isOpen = open.has(path);
        return (
          <div key={it.name}>
            <button
              aria-expanded={isOpen}
              className="flex h-6 w-full min-w-0 items-center gap-1 pr-2 text-left text-(--fg-2) hover:bg-(--bg-chip)"
              onClick={() => setOpen((o) => (o.has(path) ? new Set([...o].filter((p) => p !== path)) : new Set([...o, path])))}
              style={{ paddingLeft: 6 + depth * 14 }}
              type="button"
            >
              {isOpen ? <ChevronDownIcon className="size-3 shrink-0" /> : <ChevronRightIcon className="size-3 shrink-0" />}
              <FolderIcon className="size-3 shrink-0 text-(--fg-dim)" />
              <span className="min-w-0 truncate">{it.name}</span>
            </button>
            {isOpen && <AllFiles changed={changed} depth={depth + 1} dir={`${path}/`} onPick={onPick} picked={picked} streamId={streamId} />}
          </div>
        );
      })}
    </>
  );
}

/** A file as it is on disk now, highlighted, its changes marked in the
 *  gutter; a mark opens who wrote that stretch, and Ask. */
function ReadFile({ streamId, path, entry, sources, target, stamp, onAsk, onRead }: {
  streamId: string;
  path: string;
  entry: CodeEntry | null;
  sources: ReadonlyMap<string, HunkSource>;
  target: { line: number; at: number } | null;
  stamp: string;
  onAsk: (hunk: Hunk) => void;
  /** Whether the node says the file just read is a secret. */
  onRead: (secret: boolean) => void;
}) {
  const [file, setFile] = useState<{ text: string; secret: boolean } | { error: string } | null>(null);
  useSecretShown(!!file && "text" in file && file.secret);
  const read = useRef(onRead);
  read.current = onRead;
  useEffect(() => {
    let live = true;
    filesRead({ stream_id: streamId, path }).then(
      (f) => {
        if (!live) return;
        setFile({ text: f.text, secret: f.secret });
        read.current(f.secret);
      },
      (e) => live && setFile({ error: said(e) }),
    );
    return () => {
      live = false;
    };
  }, [streamId, path, stamp]);
  const text = file && "text" in file ? file.text : null;
  // ponytail: a very large file reads plain; highlight it in pieces if that matters.
  const hl = useHighlighted(text ?? "", text && text.length < 400_000 ? languageForPath(path) : undefined);
  const lines = useMemo(() => {
    const all = (text ?? "").split("\n");
    if (all.length > 1 && all.at(-1) === "") all.pop();
    return all;
  }, [text]);
  const { marks, stretches } = useMemo(() => {
    if (entry?.untracked) return { marks: new Map<number, Mark>(lines.map((_, i) => [i + 1, "add"])), stretches: [] };
    return gutterOf(entry ? diskHunks(entry) : []);
  }, [entry, lines]);
  const loose = !!entry && (entry.uncommitted.length > 0 || entry.untracked);
  const [card, setCard] = useState<number | null>(null);
  const [flash, setFlash] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!target || text === null) return;
    box.current?.querySelector(`[data-line="${target.line}"]`)?.scrollIntoView({ block: "center" });
    setFlash(target.line);
    const t = setTimeout(() => setFlash(0), 1600);
    return () => clearTimeout(t);
  }, [target, text]);

  if (file && "error" in file) {
    return <p className="p-3 text-(--fg-dim)">{entry?.status === "deleted" ? "Deleted on this branch" : file.error}</p>;
  }
  if (text === null) return <p className="p-3 text-(--fg-faint)">Reading…</p>;
  const stretchAt = new Map(stretches.flatMap((s) => Array.from({ length: s.end - s.start + 1 }, (_, i) => [s.start + i, s] as const)));
  return (
    <div className="min-h-0 flex-1 overflow-auto py-2 font-mono text-[12px] leading-5" ref={box}>
      {lines.map((line, i) => {
        const n = i + 1;
        const mark = marks.get(n);
        const s = stretchAt.get(n);
        return (
          <div key={n}>
            <div
              className="flex min-w-max"
              data-line={n}
              style={{ background: n === flash ? "var(--accent-sel)" : mark ? ROW_TINT[mark] : undefined }}
            >
              <span className="w-12 shrink-0 select-none pr-3 text-right text-(--fg-linenum)">{n}</span>
              {mark && s ? (
                <button
                  aria-label={`Who wrote lines ${s.start}–${s.end}`}
                  className="mr-2.5 w-[3px] shrink-0 cursor-pointer hover:mr-2 hover:w-[5px]"
                  onClick={() => setCard((c) => (c === s.start ? null : s.start))}
                  style={{ background: MARK[mark] }}
                  type="button"
                />
              ) : (
                <span className="mr-2.5 w-[3px] shrink-0" style={{ background: mark ? MARK[mark] : undefined }} />
              )}
              <span className="whitespace-pre pr-4 text-(--fg-body)">
                {hl?.tokens[i]
                  ? hl.tokens[i].map((t, j) => (
                      <span key={j} style={tokenStyle(t)}>
                        {t.content}
                      </span>
                    ))
                  : line}
              </span>
            </div>
            {s && card === s.start && n === s.end && (
              <div className="my-1 mr-3 ml-[62px] flex items-center gap-2 rounded-md border border-(--line) bg-(--bg-raised) px-2.5 py-1.5 font-sans text-[11px]">
                <Chip>{s.start === s.end ? `LINE ${s.start}` : `LINES ${s.start}–${s.end}`}</Chip>
                {sources.has(`${path}:${s.hunk.newStart}`) ? (
                  <Provenance source={sources.get(`${path}:${s.hunk.newStart}`)} />
                ) : (
                  loose && <span className="text-(--fg-dim)">not committed</span>
                )}
                <span className="flex-1" />
                <button className={`${BUTTON} h-5`} onClick={() => onAsk(s.hunk)} type="button">
                  Ask
                </button>
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Where else the file opens: Code (code-server, Bakery-styled) first, then
 *  this Mac's editors, the last one used first. */
function OpenIn({ ctx, streamId, folder, file, line, working }: {
  ctx: ToolContext;
  streamId: string;
  folder: string;
  file: string;
  line: number;
  working: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [editors, setEditors] = useState<{ id: string; name: string; icon: string }[]>([]);
  const [error, setError] = useState("");
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));
  useEffect(() => {
    if (!open) return;
    electronShell()
      ?.editors()
      .then(setEditors, () => setEditors([]));
  }, [open]);
  const item = "flex h-7 w-full items-center gap-2 rounded px-2 text-left text-(--fg-body) hover:bg-(--bg-chip)";
  const external = async (id: string) => {
    setOpen(false);
    setError("");
    if (working && !(await ask("The agent is working here: what you save now lands in its turn."))) return;
    electronShell()
      ?.openInEditor(id, { folder, file, line })
      .catch((e) => setError(said(e)));
  };
  return (
    <div className="relative" ref={box}>
      <button aria-expanded={open} aria-haspopup="menu" className={BUTTON} onClick={() => setOpen((v) => !v)} title={error || undefined} type="button">
        Open in
        <ChevronDownIcon className="size-3" />
      </button>
      {open && (
        <div className={`${POP} w-56`} role="menu">
          <button
            className={item}
            onClick={() => {
              setOpen(false);
              ctx.setLayout((l) => L.openPanel(l, ctx.workspace.id, `@editor:${streamId}`, 900));
            }}
            role="menuitem"
            type="button"
          >
            <CodeIcon className="size-4 text-(--hue-containers)" />
            Code
          </button>
          {editors.map((e) => (
            <button className={item} key={e.id} onClick={() => void external(e.id)} role="menuitem" type="button">
              <img alt="" className="size-4" src={e.icon} />
              {e.name}
            </button>
          ))}
        </div>
      )}
      {error && <span className="absolute top-full right-0 mt-1 w-64 text-right text-(--bad) text-[11px]">{error}</span>}
    </div>
  );
}
