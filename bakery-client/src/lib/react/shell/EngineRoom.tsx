// The engine room (Round 8's V1, the operator's pick): what this Mac runs
// for the Bakery as one tree, with the part picked beside it. This Mac
// itself (Amphetamine, its heat and fans, Macs Fan Control, which engine the
// Bakery's Docker runs on), its disk (what each engine and the threads'
// build folders take, cleaned only when the operator says), Apple
// container, Colima (its memory against its use, the size the operator
// picks, its idle stop) and OrbStack (installed on the operator's yes),
// with their containers in the Containers page's table and drawer;
// Homebrew's services, Tailscale and its machines, cloudflared and
// Supabase's local stacks. Other machines join the tree as the node learns
// them.

import { type ReactNode, useCallback, useEffect, useState, useSyncExternalStore } from "react";
import { FanIcon, HardDriveIcon, MonitorIcon, ThermometerIcon } from "lucide-react";
import {
  type Amphetamine,
  type Backend,
  type CleanReq,
  type ContainerUse,
  disk,
  diskClean,
  type DiskReport,
  engine,
  engineColimaIdleStop,
  engineColimaSize,
  engineDockerDefault,
  engineOrbstackInstall,
  type EngineRoom as Room,
  type Heat,
  macAmphetamine,
  macAmphetamineSet,
  macFansOpen,
  macHeat,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { onEnvelope } from "$lib/events";
import { keep, refresh, useCached } from "./cache";
import { ContainersPanel } from "./ContainersPanel";
import { bytes } from "./containersModel";
import { PLAIN, SOLID } from "./Internet";
import { Chip, type ChipTone, Freshness, said, type ToolContext, usePoll } from "./ui";

type Pick = "mac" | "disk" | "apple" | "orbstack" | "brew" | "tailscale" | "cloudflared" | "supabase" | `colima:${string}`;

const FIVE_MINUTES = 5 * 60_000;

/** A part to open the engine room on, for when it isn't open yet. */
let pending: Pick | null = null;
/** The part it shows, kept when it mounts again or opens again. */
let shown: Pick = "mac";

/** OrbStack's install as the node last told it, heard whether or not the
 *  engine room is open, so a mount again keeps it; once it's installed the
 *  room reads again. */
let orbInstall: { status: string; said: string } | null = null;
const orbListeners = new Set<() => void>();
let orbHearing = false;
function setOrbInstall(next: { status: string; said: string }) {
  orbInstall = next;
  for (const listener of orbListeners) listener();
}
function useOrbInstall() {
  if (!orbHearing) {
    orbHearing = true;
    onEnvelope("engine-install", (e) => {
      if (e.engine !== "orbstack") return;
      setOrbInstall({ status: String(e.status ?? ""), said: String(e.said ?? "") });
      if (e.status === "installed") void refresh("engine", () => engine());
    });
  }
  return useSyncExternalStore(
    (listener) => {
      orbListeners.add(listener);
      return () => orbListeners.delete(listener);
    },
    () => orbInstall,
  );
}
/** Opens the engine room where the operator is, on one part of it. */
export function openEngineRoomOn(part: Pick) {
  pending = part;
  window.dispatchEvent(new CustomEvent("bakery:engine-pick", { detail: part }));
  window.dispatchEvent(new CustomEvent("bakery:open-tool", { detail: { id: "@engine" } }));
}
/** The disk report, shared by the engine room and the projects' Disk cards;
 *  it measures folders, so it's read again only after a while. */
export const useDisk = () => useCached<DiskReport>("disk", () => disk(), { maxAge: FIVE_MINUTES });
const useRoom = () => useCached<Room>("engine", () => engine());

const Dot = ({ on }: { on: boolean }) => <span className={`size-2 shrink-0 rounded-full ${on ? "bg-(--ok)" : "bg-(--fg-faint)"}`} />;

function TreeRow({ depth, on, onPick, children, right }: { depth: number; on: boolean; onPick: () => void; children: ReactNode; right?: ReactNode }) {
  return (
    <button
      aria-pressed={on}
      className={`flex h-7 min-w-0 items-center gap-2 rounded-[5px] pr-2 text-left ${on ? "bg-(--accent-sel) font-bold text-(--fg)" : "text-(--fg-body) hover:bg-(--bg-chip)"}`}
      onClick={onPick}
      style={{ paddingLeft: 8 + depth * 14 }}
      type="button"
    >
      {children}
      <span className="flex-1" />
      {right}
    </button>
  );
}

const THERMAL: Record<string, [ChipTone, string]> = {
  nominal: ["ok", "COOL"],
  fair: ["warn", "WARM"],
  serious: ["bad", "HOT"],
  critical: ["bad", "CRITICAL"],
};

function Section({ title, icon, action, children }: { title: string; icon?: ReactNode; action?: ReactNode; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2 rounded-lg border border-(--line) bg-(--bg-raised) p-3">
      <div className="flex items-center gap-2 font-bold text-(--fg-dim) text-[10.5px] tracking-[.08em]">
        {icon}
        {title}
        <span className="flex-1" />
        <span className="font-normal tracking-normal">{action}</span>
      </div>
      {children}
    </section>
  );
}

/** Parts of a whole, as one bar: each part its colour, the rest empty. */
function Bar({ total, parts }: { total: number; parts: [number, string, string][] }) {
  return (
    <div className="flex h-2.5 w-full overflow-hidden rounded-full bg-(--bg-chip)">
      {parts.map(([n, hue, label]) => (
        <span key={label} style={{ width: `${total > 0 ? Math.min(100, (n / total) * 100) : 0}%`, background: hue }} title={`${label}: ${bytes(n)}`} />
      ))}
    </div>
  );
}

const TABLE = "w-full border-separate border-spacing-0 text-[11.5px]";
const TH = "border-(--line) border-b px-2 py-1 text-left font-bold text-(--fg-dim) text-[10px] tracking-[.06em]";
const TD = "border-(--line-row) border-b px-2 py-1 text-(--fg-body)";

function UseTable({ rows }: { rows: ContainerUse[] }) {
  if (rows.length === 0) return null;
  return (
    <table className={TABLE}>
      <thead>
        <tr>
          <th className={TH}>CONTAINER</th>
          <th className={`${TH} text-right`}>MEMORY</th>
          <th className={`${TH} text-right`}>CPU</th>
        </tr>
      </thead>
      <tbody>
        {[...rows]
          .sort((a, b) => b.memory - a.memory)
          .map((c) => (
            <tr key={c.name}>
              <td className={`${TD} font-mono text-(--fg)`}>{c.name}</td>
              <td className={`${TD} text-right`}>{bytes(c.memory)}</td>
              <td className={`${TD} text-right`}>{c.cpu_percent.toFixed(1)}%</td>
            </tr>
          ))}
      </tbody>
    </table>
  );
}

/** This Mac: which engine the Bakery's Docker runs on, Amphetamine keeping
 *  it awake (with the lid shut too), and its heat, which holds agents' new
 *  turns while it runs hot. */
function ThisMacPane({ room, onRoom }: { room: Room | null; onRoom: () => void }) {
  const amph = useCached<Amphetamine>("mac:amphetamine", () => macAmphetamine());
  const heat = useCached<Heat>("mac:heat", () => macHeat());
  usePoll(useCallback(() => void amph.reload(), [amph.reload]), 15_000);
  usePoll(useCallback(() => void heat.reload(), [heat.reload]), 10_000);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [note, setNote] = useState("");
  const set = (action: string) => {
    setBusy(true);
    setError("");
    macAmphetamineSet({ action }).then(
      (a) => {
        keep("mac:amphetamine", a);
        setBusy(false);
      },
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  const dockerOn = async (to: string) => {
    if (!room || room.docker_default === to) return;
    if (!(await ask(`Run the Bakery's Docker on ${to === "orbstack" ? "OrbStack" : "Colima"}, where a project picks no engine?`))) return;
    setError("");
    engineDockerDefault({ engine: to }).then(
      (d) => {
        setNote(d.note ?? "");
        onRoom();
      },
      (e) => setError(said(e)),
    );
  };
  const openFans = () => macFansOpen().then(() => setError(""), (e) => setError(said(e)));
  const a = amph.data;
  const h = heat.data;
  const [tone, word] = THERMAL[h?.thermal ?? ""] ?? ["neutral", (h?.thermal ?? "…").toUpperCase()];
  return (
    <div className="flex max-w-[680px] flex-col gap-3 p-4" data-testid="this-mac">
      {room && (
        <Section title="DOCKER RUNS ON">
          <div className="flex items-center gap-2">
            <span className="flex overflow-hidden rounded-[5px] border border-(--line)">
              {(["colima", "orbstack"] as const).map((id, n) => (
                <button
                  className={`h-6 px-2.5 text-[11px] disabled:opacity-40 ${n > 0 ? "border-(--line) border-l" : ""} ${room.docker_default === id ? "bg-(--accent-sel) font-bold text-(--fg)" : "text-(--fg-dim) hover:text-(--fg-2)"}`}
                  disabled={id === "orbstack" && !room.orbstack}
                  key={id}
                  onClick={() => void dockerOn(id)}
                  title={id === "orbstack" && !room.orbstack ? "OrbStack isn't installed" : undefined}
                  type="button"
                >
                  {id === "colima" ? "Colima" : "OrbStack"}
                </button>
              ))}
            </span>
            <span className="text-(--fg-dim) text-[11px]">where a project picks none</span>
          </div>
          {note && <p className="text-(--fg-body) text-[11px]">{note}</p>}
        </Section>
      )}
      <Section icon={<MonitorIcon className="size-3.5" />} title="AMPHETAMINE" action={<Freshness at={amph.at} busy={amph.busy} error={amph.error} onReload={() => void amph.reload()} />}>
        {!a ? (
          <p className="text-(--fg-faint)">{amph.error || "Reading…"}</p>
        ) : !a.installed ? (
          <p className="text-(--fg-dim)">Amphetamine isn't installed on this Mac.</p>
        ) : (
          <>
            <div className="flex items-center gap-2">
              <Dot on={a.active} />
              <span className="text-(--fg)">{a.active ? "Keeping this Mac awake" : "Not keeping this Mac awake"}</span>
              {a.active && <span className="text-(--fg-dim)">{a.seconds_left ? `${Math.ceil(a.seconds_left / 60)} min left` : "no end"}</span>}
              <span className="flex-1" />
              {a.active ? (
                <button className={PLAIN} disabled={busy} onClick={() => set("end")} type="button">
                  End
                </button>
              ) : (
                <button className={SOLID} disabled={busy} onClick={() => set("start")} type="button">
                  Start
                </button>
              )}
            </div>
            <div className="flex items-center gap-2.5">
              <Switch disabled={busy} label="Awake with the lid shut" on={a.closed_display} onChange={() => set(a.closed_display ? "closed_display_off" : "closed_display_on")} />
              <span className="text-(--fg)">Awake with the lid shut</span>
            </div>
          </>
        )}
      </Section>
      <Section
        action={
          <span className="flex items-center gap-1.5">
            <Freshness at={heat.at} busy={heat.busy} error={heat.error} onReload={() => void heat.reload()} />
            <button className={PLAIN} onClick={() => void openFans()} type="button">
              <FanIcon className="size-3" />
              Open Macs Fan Control
            </button>
          </span>
        }
        icon={<ThermometerIcon className="size-3.5" />}
        title="HEAT"
      >
        {!h ? (
          <p className="text-(--fg-faint)">{heat.error || "Reading…"}</p>
        ) : (
          <>
            <div className="flex flex-wrap items-center gap-2">
              <Chip tone={tone}>{word}</Chip>
              {typeof h.hottest_c === "number" && <span className="text-(--fg)">hottest sensor {Math.round(h.hottest_c)} °C</span>}
              {h.holding && <Chip tone="bad">NEW TURNS WAIT FOR IT TO COOL</Chip>}
            </div>
            {h.fans.length > 0 && (
              <div className="flex flex-col gap-0.5 text-(--fg-body)">
                {h.fans.map((f, i) => (
                  <span className="flex items-center gap-2" key={`fan-${i}`}>
                    <FanIcon className="size-3 text-(--fg-dim)" />
                    Fan {i + 1}: {Math.round(f.rpm)} rpm
                    {typeof f.min === "number" && typeof f.max === "number" && (
                      <span className="text-(--fg-dim)">
                        ({Math.round(f.min)}–{Math.round(f.max)})
                      </span>
                    )}
                  </span>
                ))}
              </div>
            )}
          </>
        )}
      </Section>
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </div>
  );
}

/** Homebrew services the room shows live in rows of their own: brew's word
 *  for them is its launchd job's, not the engine's (Apple container's job
 *  exits once the engine is up, Colima's VMs never run as one), so it said
 *  "stopped" and "none" beside engines that run. */
const OWN_ROW = new Set(["container", "colima", "orbstack", "tailscale", "cloudflared"]);

export function Switch({ on, disabled, label, onChange }: { on: boolean; disabled?: boolean; label: string; onChange: () => void }) {
  return (
    <button
      aria-checked={on}
      aria-label={label}
      className={`relative h-[18px] w-8 shrink-0 rounded-full disabled:opacity-50 ${on ? "bg-(--accent-solid)" : "bg-(--bg-chip)"}`}
      disabled={disabled}
      onClick={onChange}
      role="switch"
      type="button"
    >
      <span className={`absolute top-0.5 size-3.5 rounded-full bg-(--bg-app) ${on ? "left-4" : "left-0.5"}`} />
    </button>
  );
}

const ENGINE_DISK: { key: "docker" | "orbstack" | "apple_container"; backend: Backend; label: string }[] = [
  { key: "docker", backend: "docker", label: "Docker (Colima)" },
  { key: "orbstack", backend: "orbstack", label: "OrbStack" },
  { key: "apple_container", backend: "apple", label: "Apple container" },
];
const CLEAN: Record<string, [CleanReq["what"], string] | undefined> = {
  images: ["unused_images", "Remove unused"],
  containers: ["stopped_containers", "Remove stopped"],
  "build cache": ["build_cache", "Clear"],
};

/** The disk: what each engine and the threads' build folders take, and
 *  what can go; nothing goes without the operator's yes, and a volume only
 *  when ticked. The Bakery's own data is listed, never offered. */
function DiskPane() {
  const d = useDisk();
  const r = d.data;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [told, setTold] = useState("");
  const [volumes, setVolumes] = useState<Backend | null>(null);
  const [ticked, setTicked] = useState<Set<string>>(new Set());
  const clean = async (req: CleanReq, question: string) => {
    if (!(await ask(question))) return;
    setBusy(true);
    setError("");
    setTold("");
    diskClean(req).then(
      (c) => {
        setBusy(false);
        setTold([c.output.trim(), `${bytes(c.free)} free now`].filter(Boolean).join("\n"));
        setTicked(new Set());
        void d.reload();
      },
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  if (!r) return <p className="p-4 text-(--fg-faint)">{d.error || "Measuring…"}</p>;
  const used = r.total - r.free;
  return (
    <div className="flex max-w-[860px] flex-col gap-3 p-4" data-testid="disk">
      <Section
        action={<Freshness at={d.at} busy={d.busy} error={d.error} onReload={() => void d.reload()} />}
        icon={<HardDriveIcon className="size-3.5" />}
        title="DISK"
      >
        <div className="flex items-center gap-2">
          <span className="text-(--fg)">
            {bytes(r.free)} free of {bytes(r.total)}
          </span>
          {r.low && <Chip tone="bad">LOW</Chip>}
        </div>
        <Bar parts={[[used, r.low ? "var(--bad)" : "var(--accent)", "used"]]} total={r.total} />
      </Section>
      {ENGINE_DISK.map(({ key, backend, label }) => {
        const uses = r[key];
        if (!uses || uses.length === 0) return null;
        const vols = r.items.filter((i) => i.engine === backend && i.kind === "volume" && i.state === "unused" && i.project !== "Bakery");
        return (
          <Section key={key} title={label.toUpperCase()}>
            <table className={TABLE}>
              <thead>
                <tr>
                  <th className={TH}>KIND</th>
                  <th className={`${TH} text-right`}>COUNT</th>
                  <th className={`${TH} text-right`}>IN USE</th>
                  <th className={`${TH} text-right`}>SIZE</th>
                  <th className={`${TH} text-right`}>CAN GO</th>
                  <th className={TH} />
                </tr>
              </thead>
              <tbody>
                {uses.map((u) => {
                  const act = CLEAN[u.kind];
                  return (
                    <tr key={u.kind}>
                      <td className={`${TD} text-(--fg)`}>{u.kind}</td>
                      <td className={`${TD} text-right`}>{u.count}</td>
                      <td className={`${TD} text-right`}>{u.active}</td>
                      <td className={`${TD} text-right`}>{bytes(u.bytes)}</td>
                      <td className={`${TD} text-right`}>{bytes(u.reclaimable)}</td>
                      <td className={`${TD} text-right`}>
                        {act && u.reclaimable > 0 && (
                          <button
                            className={PLAIN}
                            disabled={busy}
                            onClick={() => void clean({ backend, what: act[0] } as CleanReq, `${act[1]} on ${label}? Up to ${bytes(u.reclaimable)} goes.`)}
                            type="button"
                          >
                            {act[1]}
                          </button>
                        )}
                        {u.kind === "volumes" && vols.length > 0 && (
                          <button className={PLAIN} onClick={() => setVolumes((v) => (v === backend ? null : backend))} type="button">
                            Choose…
                          </button>
                        )}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
            {volumes === backend && (
              <div className="flex flex-col gap-1 rounded-md border border-(--line) bg-(--bg-app) p-2">
                {vols.map((v) => {
                  const k = `${backend}:${v.name}`;
                  return (
                    <label className="flex items-center gap-2 text-[11px]" key={k}>
                      <input
                        checked={ticked.has(k)}
                        onChange={() =>
                          setTicked((t) => {
                            const next = new Set(t);
                            if (!next.delete(k)) next.add(k);
                            return next;
                          })
                        }
                        type="checkbox"
                      />
                      <span className="min-w-0 flex-1 truncate font-mono text-(--fg)">{v.name}</span>
                      <span className="text-(--fg-dim)">{[v.group, v.project].filter(Boolean).join(" · ")}</span>
                      {typeof v.bytes === "number" && <span className="text-(--fg-dim)">{bytes(v.bytes)}</span>}
                    </label>
                  );
                })}
                <div className="flex justify-end">
                  <button
                    className={SOLID}
                    disabled={busy || ![...ticked].some((k) => k.startsWith(`${backend}:`))}
                    onClick={() => {
                      const names = [...ticked].filter((k) => k.startsWith(`${backend}:`)).map((k) => k.slice(backend.length + 1));
                      void clean({ backend, names, what: "volumes" }, `Remove ${names.length} ${names.length === 1 ? "volume" : "volumes"} on ${label}? Their data goes with them.`);
                    }}
                    type="button"
                  >
                    Remove ticked
                  </button>
                </div>
              </div>
            )}
          </Section>
        );
      })}
      {r.build_folders.length > 0 && (
        <Section title="BUILD FOLDERS">
          <table className={TABLE}>
            <tbody>
              {r.build_folders.map((f) => (
                <tr key={f.path}>
                  <td className={`${TD} text-(--fg)`}>{f.thread}</td>
                  <td className={TD}>{f.made_by}</td>
                  <td className={`${TD} max-w-72 truncate font-mono text-(--fg-dim) text-[10.5px]`} title={f.path}>
                    {f.path}
                  </td>
                  <td className={`${TD} text-right`}>{bytes(f.bytes)}</td>
                  <td className={`${TD} text-right`}>
                    <button className={PLAIN} disabled={busy} onClick={() => void clean({ path: f.path, what: "build_folder" }, `Clear ${f.made_by}'s build folder in ${f.thread}? ${bytes(f.bytes)} goes; the next build makes it again.`)} type="button">
                      Clean
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </Section>
      )}
      {(r.bakery.length > 0 || typeof r.colima === "number") && (
        <Section title="THE BAKERY'S OWN">
          <div className="flex flex-col gap-0.5 text-[11px]">
            {typeof r.colima === "number" && (
              <span className="flex justify-between gap-2">
                <span className="text-(--fg)">Colima's VMs</span>
                <span className="text-(--fg-dim)">{bytes(r.colima)}</span>
              </span>
            )}
            {r.bakery.map((p) => (
              <span className="flex justify-between gap-2" key={p.name}>
                <span className="font-mono text-(--fg)">{p.name}</span>
                <span className="text-(--fg-dim)">{bytes(p.bytes)}</span>
              </span>
            ))}
          </div>
        </Section>
      )}
      {told && <pre className="whitespace-pre-wrap rounded-md bg-(--bg-app) p-2 text-(--fg-body) text-[11px]">{told}</pre>}
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </div>
  );
}

/** Colima's default VM: its memory against what its programs use, the size
 *  the operator picks for its next start, and its idle stop. */
function ColimaPane({ room, onRoom }: { room: Room; onRoom: () => void }) {
  const u = room.colima_use;
  const vm = room.colima?.find((v) => v.name === "default");
  const [gib, setGib] = useState<string>(() => String(room.colima_next_size?.memory_gib ?? room.colima_suggested_gib ?? ""));
  const [cpus, setCpus] = useState<string>(() => (room.colima_next_size?.cpus ? String(room.colima_next_size.cpus) : ""));
  const [error, setError] = useState("");
  const size = (next: { memory_gib: number; cpus: number | null } | null) => {
    setError("");
    engineColimaSize({ size: next }).then(onRoom, (e) => setError(said(e)));
  };
  const idle = (on: boolean) => engineColimaIdleStop({ on }).then(onRoom, (e) => setError(said(e)));
  return (
    <Section title="COLIMA · DEFAULT">
      <div className="flex flex-wrap items-center gap-2">
        <Dot on={!!vm?.running} />
        <span className="text-(--fg)">{vm?.running ? "running" : room.colima_idle_stopped ? "stopped while idle" : "stopped"}</span>
        {vm && typeof vm.memory === "number" && (
          <span className="text-(--fg-dim)">
            {bytes(vm.memory)}
            {typeof vm.cpus === "number" ? ` · ${vm.cpus} CPUs` : ""}
          </span>
        )}
        {room.colima_idle_stopped && <span className="text-(--fg-dim) text-[11px]">a Docker action starts it</span>}
      </div>
      {u && (
        <div className="flex flex-col gap-1">
          <Bar
            parts={[
              [u.guest_programs, "var(--accent)", "programs"],
              [u.guest_cache, "color-mix(in srgb, var(--accent) 35%, transparent)", "cache"],
            ]}
            total={u.guest_total}
          />
          <span className="text-(--fg-dim) text-[11px]">
            programs {bytes(u.guest_programs)} · cache {bytes(u.guest_cache)} · free {bytes(u.guest_free)} of {bytes(u.guest_total)}
            {typeof u.on_mac === "number" ? ` · holds ${bytes(u.on_mac)} of this Mac` : ""}
          </span>
          <UseTable rows={u.containers} />
        </div>
      )}
      <div className="flex flex-wrap items-center gap-2 text-[11px]">
        {typeof room.colima_peak === "number" && <span className="text-(--fg-dim)">peak this fortnight {bytes(room.colima_peak)}</span>}
        {typeof room.colima_suggested_gib === "number" && <Chip tone="accent">SUGGESTED {room.colima_suggested_gib} GiB</Chip>}
        {room.colima_next_size && (
          <>
            <Chip tone="warn">
              NEXT START {room.colima_next_size.memory_gib} GiB{room.colima_next_size.cpus ? ` · ${room.colima_next_size.cpus} CPUs` : ""}
            </Chip>
            <button className={PLAIN} onClick={() => size(null)} type="button">
              Take back
            </button>
          </>
        )}
      </div>
      <form
        className="flex flex-wrap items-center gap-1.5"
        onSubmit={(e) => {
          e.preventDefault();
          const memory = Number(gib);
          if (memory > 0) size({ memory_gib: memory, cpus: Number(cpus) > 0 ? Number(cpus) : null });
        }}
      >
        <input aria-label="Memory, GiB" className="h-6 w-16 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-[11px]" min={1} onChange={(e) => setGib(e.target.value)} type="number" value={gib} />
        <span className="text-(--fg-dim) text-[11px]">GiB</span>
        <input aria-label="CPUs" className="h-6 w-14 rounded-[5px] border border-(--line) bg-(--bg-app) px-2 text-[11px]" min={1} onChange={(e) => setCpus(e.target.value)} placeholder="CPUs" type="number" value={cpus} />
        <button className={SOLID} disabled={!(Number(gib) > 0)} type="submit">
          Use at the next start
        </button>
      </form>
      <div className="flex items-center gap-2.5">
        <Switch label="Stop while idle" on={room.colima_idle_stop} onChange={() => void idle(!room.colima_idle_stop)} />
        <span className="text-(--fg)">Stop while idle</span>
      </div>
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </Section>
  );
}

/** OrbStack: installed on the operator's yes, its memory and containers. */
function OrbStackPane({ room, ctx }: { room: Room; ctx: ToolContext }) {
  const install = useOrbInstall();
  const [error, setError] = useState("");
  const o = room.orbstack;
  if (!o) {
    return (
      <div className="flex max-w-[680px] flex-col gap-3 p-4">
        <Section title="ORBSTACK">
          <p className="text-(--fg-dim)">OrbStack isn't installed on this Mac.</p>
          <div className="flex items-center gap-2">
            <button
              className={SOLID}
              disabled={install?.status === "installing"}
              onClick={async () => {
                if (!(await ask("Install OrbStack with Homebrew?"))) return;
                setError("");
                engineOrbstackInstall().then(() => setOrbInstall({ status: "installing", said: "" }), (e) => setError(said(e)));
              }}
              type="button"
            >
              {install?.status === "installing" ? "Installing…" : "Install OrbStack"}
            </button>
            {install?.status === "failed" && <Chip tone="bad">FAILED</Chip>}
          </div>
          {install?.said && <pre className="whitespace-pre-wrap text-(--bad) text-[11px]">{install.said}</pre>}
          {error && <p className="text-(--bad) text-[11px]">{error}</p>}
        </Section>
      </div>
    );
  }
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="p-4 pb-0">
        <Section title="ORBSTACK">
          <div className="flex flex-wrap items-center gap-2">
            <Dot on={o.running} />
            <span className="text-(--fg)">{o.running ? "running" : "stopped"}</span>
            <span className="text-(--fg-dim)">
              {typeof o.memory === "number" ? `up to ${bytes(o.memory)}` : ""}
              {typeof o.cpus === "number" ? ` · ${o.cpus} CPUs` : ""}
              {typeof o.on_mac === "number" ? ` · holds ${bytes(o.on_mac)} of this Mac now` : ""}
            </span>
          </div>
          <UseTable rows={o.containers} />
        </Section>
      </div>
      {o.running && <ContainersPanel ctx={ctx} only="orbstack" />}
    </div>
  );
}

export function EngineRoom({ ctx }: { ctx: ToolContext }) {
  const room = useRoom();
  const { reload } = room;
  usePoll(useCallback(() => void reload(), [reload]), 15_000);
  const d = useDisk();
  const [pick, setPicked] = useState<Pick>(() => {
    shown = pending ?? shown;
    pending = null;
    return shown;
  });
  const setPick = (p: Pick) => {
    shown = p;
    setPicked(p);
  };
  useEffect(() => {
    const onPick = (ev: Event) => {
      pending = null;
      setPick((ev as CustomEvent<Pick>).detail);
    };
    window.addEventListener("bakery:engine-pick", onPick);
    return () => window.removeEventListener("bakery:engine-pick", onPick);
  }, []);
  const e = room.data;
  const onRoom = useCallback(() => void reload(), [reload]);
  const brew = e?.brew_services.filter((s) => !OWN_ROW.has(s.name)) ?? [];
  const started = brew.filter((s) => s.status === "started").length;
  const vm = pick.startsWith("colima:") ? (e?.colima?.find((v) => `colima:${v.name}` === pick) ?? null) : null;
  const row = (p: Pick, depth: number, label: ReactNode, right?: ReactNode) => (
    <TreeRow depth={depth} key={p} on={pick === p} onPick={() => setPick(p)} right={right}>
      {label}
    </TreeRow>
  );
  const dim = (t: string) => <span className="text-(--fg-dim) text-[10.5px]">{t}</span>;

  return (
    <div className="flex h-full min-h-0 bg-(--bg-panel) text-xs" data-testid="engine-room">
      <nav aria-label="Machines" className="flex w-64 shrink-0 flex-col gap-px overflow-auto border-(--line) border-r bg-(--bg-raised) p-2">
        <div className="flex h-7 items-center gap-1 px-2 font-bold text-(--link) text-[10px] tracking-[.08em]">
          MACHINES
          <span className="flex-1" />
          <span className="font-normal tracking-normal">
            <Freshness at={room.at} busy={room.busy} error={room.error} onReload={() => void reload()} />
          </span>
        </div>
        {row(
          "mac",
          0,
          <>
            <MonitorIcon className="size-3.5 shrink-0 text-(--link)" />
            This Mac
          </>,
        )}
        {row(
          "disk",
          1,
          <>
            <HardDriveIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
            Disk
          </>,
          d.data ? d.data.low ? <Chip tone="bad">LOW</Chip> : dim(`${bytes(d.data.free)} free`) : undefined,
        )}
        {e?.apple_container != null &&
          row(
            "apple",
            1,
            <>
              <Dot on={e.apple_container === "running"} />
              Apple container
            </>,
            e.apple_container === "running" ? undefined : dim(e.apple_container),
          )}
        {e?.colima?.map((v) =>
          row(
            `colima:${v.name}`,
            1,
            <>
              <Dot on={v.running} />
              Colima · {v.name}
            </>,
            v.name === "default" ? dim([e.docker_default === "colima" ? "Docker" : "", e.colima_idle_stopped ? "idle" : ""].filter(Boolean).join(" · ")) : undefined,
          ),
        )}
        {e &&
          row(
            "orbstack",
            1,
            <>
              <Dot on={!!e.orbstack?.running} />
              OrbStack
            </>,
            e.orbstack ? (e.docker_default === "orbstack" ? dim("Docker") : undefined) : dim("Install"),
          )}
        {brew.length > 0 &&
          row(
            "brew",
            1,
            <>
              <Dot on={started > 0} />
              Homebrew services
            </>,
            dim(`${started}/${brew.length}`),
          )}
        {e?.tailscale &&
          row(
            "tailscale",
            1,
            <>
              <Dot on={e.tailscale.connected} />
              Tailscale
            </>,
            dim(`${e.tailscale.peers.filter((p) => p.online).length} online`),
          )}
        {e?.cloudflared != null &&
          row(
            "cloudflared",
            1,
            <>
              <Dot on={e.cloudflared} />
              cloudflared
            </>,
          )}
        {e &&
          e.supabase.length > 0 &&
          row(
            "supabase",
            1,
            <>
              <Dot on />
              Supabase
            </>,
            dim(`${e.supabase.length}`),
          )}
      </nav>
      <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-auto">
        {pick === "mac" && <ThisMacPane onRoom={onRoom} room={e} />}
        {pick === "disk" && <DiskPane />}
        {pick === "apple" && <ContainersPanel ctx={ctx} only="apple" />}
        {pick === "orbstack" && e && <OrbStackPane ctx={ctx} room={e} />}
        {vm && vm.name === "default" && e && (
          <>
            <div className="max-w-[860px] p-4 pb-0">
              <ColimaPane onRoom={onRoom} room={e} />
            </div>
            <ContainersPanel ctx={ctx} only="docker" />
          </>
        )}
        {vm && vm.name !== "default" && (
          <div className="flex flex-col gap-1 p-4">
            <span className="flex items-center gap-2 font-bold text-(--fg)">
              <Dot on={vm.running} />
              Colima · {vm.name}
            </span>
            <span className="text-(--fg-dim)">
              {vm.running ? "running" : "stopped"}
              {typeof vm.cpus === "number" ? ` · ${vm.cpus} CPUs` : ""}
              {typeof vm.memory === "number" ? ` · ${bytes(vm.memory)}` : ""}
            </span>
          </div>
        )}
        {pick === "brew" && e && (
          <table className={TABLE}>
            <thead>
              <tr>
                <th className={TH}>SERVICE</th>
                <th className={TH}>STATUS</th>
              </tr>
            </thead>
            <tbody>
              {brew.map((s) => (
                <tr key={s.name}>
                  <td className={`${TD} font-mono text-(--fg)`}>{s.name}</td>
                  <td className={TD}>
                    <Chip tone={s.status === "started" ? "ok" : s.status === "error" ? "bad" : "neutral"}>{s.status.toUpperCase()}</Chip>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {pick === "tailscale" && e?.tailscale && (
          <div className="flex flex-col">
            <div className="flex items-center gap-2 px-3 py-2">
              <Dot on={e.tailscale.connected} />
              <span className="text-(--fg)">{e.tailscale.connected ? "Connected" : "Not connected"}</span>
              {e.tailscale.name && <span className="text-(--fg-dim)">as {e.tailscale.name}</span>}
            </div>
            <table className={TABLE}>
              <thead>
                <tr>
                  <th className={TH}>MACHINE</th>
                  <th className={TH}>SYSTEM</th>
                </tr>
              </thead>
              <tbody>
                {e.tailscale.peers.map((p) => (
                  <tr key={p.name}>
                    <td className={TD}>
                      <span className="flex items-center gap-2">
                        <Dot on={p.online} />
                        <span className="text-(--fg)">{p.name}</span>
                      </span>
                    </td>
                    <td className={`${TD} text-(--fg-dim)`}>{p.os}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {pick === "cloudflared" && e?.cloudflared != null && (
          <p className="flex items-center gap-2 p-4 text-(--fg)">
            <Dot on={e.cloudflared} />
            {e.cloudflared ? "cloudflared is running" : "cloudflared isn't running"}
          </p>
        )}
        {pick === "supabase" && e && (
          <div className="flex flex-col gap-1 p-4">
            {e.supabase.map((s) => (
              <span className="flex items-center gap-2 text-(--fg)" key={s}>
                <Dot on />
                {s}
              </span>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
