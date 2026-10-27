// The container manager, Docker Desktop's job: Apple container and Docker on
// Colima side by side, through the node's container routes (the same routes
// the Baker drives as tools). Lists come typed from the node; stats are
// shaped in `containersModel.ts`.
//
// ponytail: polls (lists every 5 s, stats and live logs every 3 s, paused
// while the window is hidden); switch to events if the node ever emits
// container facts.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { PlayIcon, PlusIcon, RotateCwIcon, SquareIcon, Trash2Icon, XIcon } from "lucide-react";
import {
  containersInspect,
  containersList,
  containersLogs,
  containersPrune,
  containersRemove,
  containersRestart,
  containersRun,
  containersStats,
  containersStart,
  containersStop,
  engine,
  type Backend,
  type ContainerRow,
  type EngineRoom,
  type Kind,
  type Listing,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { actorLabel } from "$lib/transcript";
import * as M from "./containersModel";
import { keep, kept, useCached } from "./cache";
import { Freshness, said, type ToolContext, usePoll } from "./ui";

const BACKENDS: { id: Backend; label: string }[] = [
  { id: "apple", label: "Apple container" },
  { id: "docker", label: "Docker (Colima)" },
  { id: "orbstack", label: "OrbStack" },
];
const TABS: { id: Kind; label: string }[] = [
  { id: "containers", label: "Containers" },
  { id: "images", label: "Images" },
  { id: "volumes", label: "Volumes" },
  { id: "networks", label: "Networks" },
  { id: "compose", label: "Compose" },
];
const KIND_WORD: Record<Kind, string> = {
  containers: "container",
  images: "image",
  volumes: "volume",
  networks: "network",
  compose: "compose project",
};

type Loaded = { listing: Listing | null; error: string };
type Picked = { backend: Backend; id: string; name: string; running: boolean };

/** A container the shell asked the Containers page to show (a sidebar
 *  row): its page takes it at once, or when it next opens. */
let wantedContainer: Picked | null = null;
const wantingContainer = new Set<() => void>();

export function revealContainer(backend: Backend, c: { id: string; name: string; running: boolean }) {
  wantedContainer = { backend, id: c.id, name: c.name, running: c.running };
  for (const take of wantingContainer) take();
}

const pill = "rounded-full border border-(--line) px-2 py-0.5 text-[11px] hover:bg-(--bg-chip)";
const iconButton = "rounded p-1 text-(--fg-faint) hover:bg-(--bg-chip) hover:text-(--fg-2) disabled:opacity-40";

/** The Containers page; `only`, one engine's part of it (the engine room). */
export function ContainersPanel({ ctx, only }: { ctx: ToolContext; only?: Backend }) {
  /** Which agent a container is for, by the node's own labels. */
  const owner = (c: ContainerRow): string => {
    const title = ctx.snap.conversations.find((v) => v.streamId === c.stream_id)?.title ?? "an agent";
    if (c.role === "seat") return `sandbox · ${title}`;
    if (c.role === "editor") return `Code · ${title}`;
    if (c.role === "run") return `started by ${actorLabel(c.started_by ?? "", (id) => ctx.snap.conversations.find((v) => v.streamId === id)?.title)}`;
    return "";
  };
  const [on, setOn] = useState<Record<Backend, boolean>>({ apple: true, docker: true, orbstack: true });
  // OrbStack once it's installed (the engine room's read, kept).
  const orbstack = !!useCached<EngineRoom>("engine", () => engine(), { maxAge: 5 * 60_000 }).data?.orbstack;
  const engines = useMemo(() => BACKENDS.filter((b) => b.id !== "orbstack" || orbstack || only === "orbstack"), [orbstack, only]);
  const [tab, setTab] = useState<Kind>("containers");
  const [lists, setLists] = useState<Partial<Record<Backend, Loaded>>>({});
  const [stats, setStats] = useState<Map<string, M.StatRow>>(new Map());
  const [picked, setPicked] = useState<Picked | null>(null);
  const [notice, setNotice] = useState("");
  const [running, setRunning] = useState(false);
  const generation = useRef(0);
  const cpu = useRef<M.AppleCpuSample>(new Map());

  useEffect(() => {
    const take = () => {
      const want = wantedContainer;
      if (!want || (only && want.backend !== only)) return;
      wantedContainer = null;
      setTab("containers");
      setOn((o) => ({ ...o, [want.backend]: true }));
      setPicked(want);
    };
    take();
    wantingContainer.add(take);
    return () => {
      wantingContainer.delete(take);
    };
  }, [only]);

  // Compose is Docker's alone.
  const shown = useMemo(() => engines.filter((b) => (only ? b.id === only : on[b.id]) && (tab !== "compose" || b.id === "docker")), [engines, on, tab, only]);

  const [at, setAt] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  // This tab's lists as last read, at once (kept across restarts).
  useEffect(() => {
    const last = shown.flatMap(({ id }) => {
      const k = kept(`containers:${id}:${tab}`);
      return k ? [[id, { listing: k.data as Loaded["listing"], error: "" }, k.at] as const] : [];
    });
    setLists(Object.fromEntries(last.map(([id, l]) => [id, l])));
    setAt(last.length ? Math.min(...last.map(([, , t]) => t)) : null);
  }, [shown, tab]);
  const load = useCallback(() => {
    const mine = ++generation.current;
    setBusy(true);
    void Promise.all(
      shown.map(async ({ id }): Promise<[Backend, Loaded]> => {
        try {
          const listing = await containersList({ backend: id, kind: tab });
          keep(`containers:${id}:${tab}`, listing);
          return [id, { listing, error: "" }];
        } catch (e) {
          return [id, { listing: null, error: `${e}` }];
        }
      }),
    ).then((pairs) => {
      if (mine !== generation.current) return;
      setLists(Object.fromEntries(pairs));
      setAt(Date.now());
      setBusy(false);
    });
  }, [shown, tab]);
  usePoll(load, 5000);

  const containers = useMemo(
    () => (tab === "containers" ? shown.flatMap(({ id }) => lists[id]?.listing?.containers ?? []) : []),
    [lists, shown, tab],
  );
  const anyRunning = containers.some((c) => c.running);
  const loadStats = useCallback(() => {
    const now = Date.now();
    void Promise.all(
      shown.map(({ id }) =>
        containersStats({ backend: id }).then(
          (res) => M.statRows(id, M.itemsOf(res), cpu.current, now),
          () => null,
        ),
      ),
    ).then((results) => {
      const rows = new Map<string, M.StatRow>();
      for (const r of results) {
        if (!r) continue;
        for (const [k, v] of r.rows) rows.set(k, v);
        for (const [k, v] of r.sample) cpu.current.set(k, v);
      }
      setStats(rows);
    });
  }, [shown]);
  usePoll(loadStats, tab === "containers" && anyRunning ? 3000 : null);

  async function act(label: string, run: () => Promise<unknown>) {
    setNotice("");
    try {
      await run();
    } catch (e) {
      setNotice(`${label}: ${said(e)}`);
    }
    load();
  }

  async function remove(backend: Backend, kind: Kind, id: string, name: string) {
    if (!(await ask(`Remove the ${KIND_WORD[kind]} ${name}?`))) return;
    if (picked?.id === id) setPicked(null);
    await act(`Remove ${name}`, () => containersRemove({ backend, kind, id }));
  }

  async function prune(backend: Backend, label: string) {
    if (!(await ask(`Remove every unused ${KIND_WORD[tab]} on ${label}?`))) return;
    await act(`Prune ${label}`, () => containersPrune({ backend, kind: tab }));
  }

  return (
    <div className="flex h-full min-h-0 flex-col text-xs" data-testid="containers-panel">
      <div className="flex flex-wrap items-center gap-1.5 border-(--line-soft) border-b px-2 py-1.5">
        {!only && engines.map((b) => (
          <button
            aria-pressed={on[b.id]}
            className={`${pill} ${on[b.id] ? "border-(--accent-border) bg-(--accent-bg) text-(--fg)" : "text-(--fg-faint)"}`}
            key={b.id}
            onClick={() => setOn((o) => ({ ...o, [b.id]: !o[b.id] }))}
            type="button"
          >
            {b.label}
          </button>
        ))}
        <span className="flex-1" />
        <button className={pill} onClick={() => setRunning((v) => !v)} type="button">
          <PlusIcon className="mr-1 inline size-3" />
          Run
        </button>
        <Freshness at={at} busy={busy} onReload={load} />
      </div>

      <div className="flex gap-3 border-(--line-soft) border-b px-2" role="tablist">
        {/* Compose is Docker's alone: no tab for another engine's own page. */}
        {TABS.filter((t) => t.id !== "compose" || !only || only === "docker").map((t) => (
          <button
            aria-selected={tab === t.id}
            className={`py-1.5 ${tab === t.id ? "border-(--accent) border-b-2 text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
            key={t.id}
            onClick={() => {
              setTab(t.id);
              setPicked(null);
            }}
            role="tab"
            type="button"
          >
            {t.label}
          </button>
        ))}
      </div>

      {running && (
        <RunForm
          onCancel={() => setRunning(false)}
          onRun={(req) =>
            act(`Run ${req.image}`, async () => {
              await containersRun(req);
              setRunning(false);
            })
          }
        />
      )}
      {notice && (
        <p className="flex items-start gap-2 border-(--bad-border) border-b bg-(--bad-bg) px-2 py-1 text-(--bad)" role="alert">
          <span className="min-w-0 flex-1 break-words">{notice}</span>
          <button aria-label="Dismiss" onClick={() => setNotice("")} type="button">
            <XIcon className="size-3" />
          </button>
        </p>
      )}

      <div className="min-h-0 flex-1 overflow-auto">
        {shown.length === 0 &&
          (tab === "compose" ? (
            <p className="flex flex-wrap items-center gap-2 p-3 text-(--fg-faint)">
              Compose runs on Docker (Colima).
              <button className={pill} onClick={() => setOn((o) => ({ ...o, docker: true }))} type="button">
                Show Docker (Colima)
              </button>
            </p>
          ) : (
            <p className="p-3 text-(--fg-faint)">Pick an engine above.</p>
          ))}
        {shown.map((b) => {
          const listing = lists[b.id];
          // The node fills only the asked kind's rows; its key is the kind.
          const rows = listing?.listing;
          const items: readonly unknown[] = rows?.[tab] ?? [];
          return (
            <section key={b.id}>
              <header className="sticky top-0 flex items-center gap-2 bg-(--bg-raised) px-2 py-1 text-(--fg-dim) text-[11px]">
                <span className="text-(--fg-2)">{b.label}</span>
                {listing && !listing.error && <span>{items.length}</span>}
                <span className="flex-1" />
                {tab !== "compose" && listing && !listing.error && (
                  <button className="text-(--fg-faint) hover:text-(--bad)" onClick={() => prune(b.id, b.label)} type="button">
                    Prune unused
                  </button>
                )}
              </header>
              {!listing && <p className="px-2 py-1.5 text-(--fg-faint)">Loading…</p>}
              {listing?.error && <p className="px-2 py-1.5 text-(--warn)">{listing.error}</p>}
              {listing && !listing.error && items.length === 0 && (
                <p className="px-2 py-1.5 text-(--fg-faint)">No {TABS.find((t) => t.id === tab)?.label.toLowerCase()}.</p>
              )}
              {tab === "containers" &&
                (rows?.containers ?? []).map((c) => {
                  const s = stats.get(c.name) ?? stats.get(c.id);
                  const who = owner(c);
                  const isPicked = picked?.backend === b.id && picked.id === c.id;
                  return (
                    <div
                      className={`group flex cursor-pointer items-center gap-2 border-(--line-row) border-b px-2 py-1 ${isPicked ? "bg-(--accent-sel)" : "hover:bg-(--bg-chip)"}`}
                      key={c.id}
                      onClick={() => setPicked({ backend: b.id, id: c.id, name: c.name, running: c.running })}
                    >
                      <span
                        aria-label={c.running ? "running" : "stopped"}
                        className="size-2 shrink-0 rounded-full"
                        style={{ background: c.running ? "var(--ok)" : "var(--idle)" }}
                      />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-1.5">
                          <span className="truncate text-(--fg)">{c.name}</span>
                          {who && (
                            <span className="shrink-0 rounded-full bg-(--bg-chip) px-1.5 text-(--fg-dim) text-[10px]">{who}</span>
                          )}
                        </span>
                        <span className="block truncate text-(--fg-faint) text-[11px]">
                          {c.image}
                          {c.ports.length ? ` · ${M.portsText(c.ports)}` : ""}
                          {c.address ? ` · ${c.address}` : ""} · {c.status}
                        </span>
                      </span>
                      {c.running && s && (
                        <span className="shrink-0 text-right text-(--fg-dim) text-[11px] tabular-nums">
                          {s.cpu && <span className="block">{s.cpu}</span>}
                          {s.mem && <span className="block">{s.mem}</span>}
                        </span>
                      )}
                      <span className="flex shrink-0 gap-0.5" onClick={(e) => e.stopPropagation()}>
                        {c.running ? (
                          <button
                            aria-label={`Stop ${c.name}`}
                            className={iconButton}
                            onClick={() => act(`Stop ${c.name}`, () => containersStop({ backend: b.id, id: c.id }))}
                            title="Stop"
                            type="button"
                          >
                            <SquareIcon className="size-3.5" />
                          </button>
                        ) : (
                          <button
                            aria-label={`Start ${c.name}`}
                            className={iconButton}
                            onClick={() => act(`Start ${c.name}`, () => containersStart({ backend: b.id, id: c.id }))}
                            title="Start"
                            type="button"
                          >
                            <PlayIcon className="size-3.5" />
                          </button>
                        )}
                        {c.running && (
                          <button
                            aria-label={`Restart ${c.name}`}
                            className={iconButton}
                            onClick={() =>
                              act(`Restart ${c.name}`, () => containersRestart({ backend: b.id, kind: "containers", id: c.id }))
                            }
                            title="Restart"
                            type="button"
                          >
                            <RotateCwIcon className="size-3.5" />
                          </button>
                        )}
                        <button
                          aria-label={`Remove ${c.name}`}
                          className={iconButton}
                          disabled={c.running}
                          onClick={() => remove(b.id, "containers", c.id, c.name)}
                          title={c.running ? "Stop it first" : "Remove"}
                          type="button"
                        >
                          <Trash2Icon className="size-3.5" />
                        </button>
                      </span>
                    </div>
                  );
                })}
              {tab === "images" &&
                (rows?.images ?? []).map((i) => (
                  <Row
                    key={i.id}
                    onRemove={() => remove(b.id, "images", i.id, i.name)}
                    primary={i.name}
                    secondary={[i.size, i.created].filter(Boolean).join(" · ")}
                  />
                ))}
              {tab === "volumes" &&
                (rows?.volumes ?? []).map((v) => (
                  <Row
                    key={v.name}
                    onRemove={() => remove(b.id, "volumes", v.name, v.name)}
                    primary={v.name}
                    // Apple reports a volume's disk-image capacity, not what it uses.
                    secondary={[v.driver, v.size && (b.id === "apple" ? `capacity ${v.size}` : v.size)].filter(Boolean).join(" · ")}
                  />
                ))}
              {tab === "networks" &&
                (rows?.networks ?? []).map((n) => (
                  <Row
                    key={n.id || n.name}
                    onRemove={() => remove(b.id, "networks", n.name, n.name)}
                    primary={n.name}
                    secondary={[n.driver, n.subnet, n.internal ? "internal" : ""].filter(Boolean).join(" · ")}
                  />
                ))}
              {tab === "compose" &&
                (rows?.compose ?? []).map((p) => (
                  <Row
                    key={p.name}
                    onRemove={() => remove("docker", "compose", p.name, p.name)}
                    onRestart={() => act(`Restart ${p.name}`, () => containersRestart({ backend: "docker", kind: "compose", id: p.name }))}
                    primary={p.name}
                    removeLabel="Take down"
                    secondary={[p.status, p.files.join(", ")].filter(Boolean).join(" · ")}
                  />
                ))}
            </section>
          );
        })}
      </div>

      {picked && tab === "containers" && <Details onClose={() => setPicked(null)} picked={picked} />}
    </div>
  );
}

function Row({
  primary,
  secondary,
  onRemove,
  onRestart,
  removeLabel = "Remove",
}: {
  primary: string;
  secondary: string;
  onRemove: () => void;
  onRestart?: () => void;
  removeLabel?: string;
}) {
  return (
    <div className="flex items-center gap-2 border-(--line-row) border-b px-2 py-1 hover:bg-(--bg-chip)">
      <span className="min-w-0 flex-1">
        <span className="block truncate text-(--fg)">{primary}</span>
        {secondary && <span className="block truncate text-(--fg-faint) text-[11px]">{secondary}</span>}
      </span>
      {onRestart && (
        <button aria-label={`Restart ${primary}`} className={iconButton} onClick={onRestart} title="Restart" type="button">
          <RotateCwIcon className="size-3.5" />
        </button>
      )}
      <button aria-label={`${removeLabel} ${primary}`} className={iconButton} onClick={onRemove} title={removeLabel} type="button">
        <Trash2Icon className="size-3.5" />
      </button>
    </div>
  );
}

function Details({ picked, onClose }: { picked: Picked; onClose: () => void }) {
  const [view, setView] = useState<"logs" | "inspect">("logs");
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const pre = useRef<HTMLPreElement>(null);

  const fetchIt = useCallback(() => {
    const req = { backend: picked.backend, id: picked.id };
    const call =
      view === "logs"
        ? containersLogs({ ...req, tail: 300 }).then((res) => String((res as { text?: unknown } | null)?.text ?? ""))
        : containersInspect({ ...req, kind: "containers" }).then((res) => JSON.stringify(res, null, 2));
    call.then(
      (t) => {
        setText(t);
        setError("");
      },
      (e) => setError(`${e}`),
    );
  }, [picked, view]);
  usePoll(fetchIt, view === "logs" && picked.running ? 3000 : 60_000);

  // Logs read from the newest line, like a terminal.
  useEffect(() => {
    if (view === "logs" && pre.current) pre.current.scrollTop = pre.current.scrollHeight;
  }, [text, view]);

  return (
    <div className="flex h-[45%] min-h-32 shrink-0 flex-col border-(--line-soft) border-t" data-testid="container-details">
      <div className="flex items-center gap-3 border-(--line-soft) border-b px-2">
        <span className="truncate py-1.5 text-(--fg)">{picked.name}</span>
        {(["logs", "inspect"] as const).map((v) => (
          <button
            aria-selected={view === v}
            className={`py-1.5 ${view === v ? "border-(--accent) border-b-2 text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
            key={v}
            onClick={() => setView(v)}
            role="tab"
            type="button"
          >
            {v === "logs" ? "Logs" : "Inspect"}
          </button>
        ))}
        <span className="flex-1" />
        <button aria-label="Close details" className={iconButton} onClick={onClose} type="button">
          <XIcon className="size-3.5" />
        </button>
      </div>
      {error && <p className="px-2 py-1 text-(--warn)">{error}</p>}
      <pre className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-all bg-(--bg-app) p-2 text-(--fg-body) text-[11px]" ref={pre}>
        {text || (error ? "" : "…")}
      </pre>
    </div>
  );
}

function RunForm({
  onRun,
  onCancel,
}: {
  onRun: (req: { backend: Backend; image: string; name: string | null; ports: { host?: number; container: number }[] }) => void;
  onCancel: () => void;
}) {
  const [backend, setBackend] = useState<Backend>("apple");
  const [image, setImage] = useState("");
  const [name, setName] = useState("");
  const [ports, setPorts] = useState("");
  const parsed = ports
    .split(",")
    .map((p) => p.trim())
    .filter(Boolean)
    .map((p) => {
      // "5432": the Bakery picks this Mac's port; "46033:5432" names one in its block.
      const [a, b] = p.split(":").map(Number);
      return b === undefined ? { container: a } : { host: a, container: b };
    });
  const portsOk = parsed.every(
    (p) => Number.isInteger(p.container) && p.container > 0 && (p.host === undefined || (Number.isInteger(p.host) && p.host > 0)),
  );
  const field = "rounded-md border border-(--line) bg-(--bg-app) px-1.5 py-0.5 outline-none focus:border-(--accent-border)";

  return (
    <form
      className="flex flex-wrap items-center gap-1.5 border-(--line-soft) border-b bg-(--bg-raised) px-2 py-1.5"
      onSubmit={(e) => {
        e.preventDefault();
        if (image.trim() && portsOk) onRun({ backend, image: image.trim(), name: name.trim() || null, ports: parsed });
      }}
    >
      <select aria-label="Runtime" className={field} onChange={(e) => setBackend(e.target.value as Backend)} value={backend}>
        {BACKENDS.map((b) => (
          <option key={b.id} value={b.id}>
            {b.label}
          </option>
        ))}
      </select>
      <input
        aria-label="Image"
        autoFocus
        className={`${field} min-w-40 flex-1`}
        onChange={(e) => setImage(e.target.value)}
        placeholder="image, e.g. docker.io/library/postgres:17"
        value={image}
      />
      <input aria-label="Name" className={`${field} w-28`} onChange={(e) => setName(e.target.value)} placeholder="name" value={name} />
      <input
        aria-invalid={!portsOk}
        aria-label="Container ports"
        className={`${field} w-32`}
        onChange={(e) => setPorts(e.target.value)}
        placeholder="5432"
        title="Published on 127.0.0.1 only, on a port the Bakery picks; comma-separate several"
        value={ports}
      />
      <button className={pill} disabled={!image.trim() || !portsOk} type="submit">
        Run
      </button>
      <button aria-label="Cancel" className={iconButton} onClick={onCancel} type="button">
        <XIcon className="size-3.5" />
      </button>
    </form>
  );
}
