// The top bar's resource pill (Round 14's P1, the operator's pick): this
// Mac's disk, memory, CPU and heat always at a glance, each with the level
// the node decides (ok, warn, bad); the worst takes the colour and says
// why, and the pill opens them all, with the CPU's last ten minutes and the
// way into the engine room.

import { useCallback, useEffect, useRef, useState } from "react";
import { CpuIcon, HardDriveIcon, MemoryStickIcon, ThermometerIcon } from "lucide-react";
import { type DiskReport, macMeters, type MeterLevel, type Meters } from "$lib/api.generated";
import { onEnvelope } from "$lib/events";
import { patchKept, useCached } from "./cache";
import { bytes } from "./containersModel";
import { openEngineRoomOn } from "./EngineRoom";
import { POP, useDismiss } from "./PanelMenu";
import { Chip, usePoll } from "./ui";

const HUE: Record<MeterLevel, string> = { ok: "var(--ok)", warn: "var(--warn)", bad: "var(--bad)" };
/** macOS's thermal states: how full the heat meter reads, and the word. */
const THERMAL: Record<string, [number, string]> = { nominal: [25, "cool"], fair: [55, "warm"], serious: [80, "hot"], critical: [100, "critical"] };
const RANK: Record<MeterLevel, number> = { ok: 0, warn: 1, bad: 2 };
type Meter = { id: string; icon: typeof HardDriveIcon; level: MeterLevel; fill: number; say: string; label: string };

/** The four meters, from the node's reading. */
function metersOf(m: Meters): Meter[] {
  const pct = (a: number, b: number) => (b > 0 ? (a / b) * 100 : 0);
  const [heatFill, heatWord] = THERMAL[m.heat.thermal ?? ""] ?? [25, m.heat.thermal ?? "…"];
  return [
    { id: "disk", icon: HardDriveIcon, level: m.disk.level, fill: pct(m.disk.total - m.disk.free, m.disk.total), say: `${bytes(m.disk.free)} free`, label: "Disk" },
    {
      id: "memory",
      icon: MemoryStickIcon,
      level: m.memory.level,
      fill: pct(m.memory.used, m.memory.total),
      say: m.memory.pressure === "normal" ? `${Math.round(pct(m.memory.used, m.memory.total))}% memory` : `memory pressure ${m.memory.pressure}`,
      label: "Memory",
    },
    { id: "cpu", icon: CpuIcon, level: m.cpu.level, fill: m.cpu.percent, say: `${Math.round(m.cpu.percent)}% CPU`, label: "CPU" },
    { id: "heat", icon: ThermometerIcon, level: m.heat.level, fill: m.heat.holding ? 100 : heatFill, say: m.heat.holding ? "hot · turns wait" : heatWord, label: "Heat" },
  ];
}

/** The CPU's last ten minutes, as a line. */
function Spark({ points }: { points: number[] }) {
  if (points.length < 2) return null;
  const step = 400 / (points.length - 1);
  const line = points.map((p, i) => `${(i * step).toFixed(1)},${(22 - (Math.min(100, Math.max(0, p)) / 100) * 20 - 1).toFixed(1)}`).join(" ");
  return (
    <svg aria-label="CPU, the last ten minutes" className="h-[22px] w-full" preserveAspectRatio="none" viewBox="0 0 400 22">
      <polyline fill="none" points={line} stroke="var(--accent)" strokeWidth="1.5" vectorEffect="non-scaling-stroke" />
    </svg>
  );
}

function Row({ icon: Icon, label, value, chip, link, onLink, children }: { icon: typeof HardDriveIcon; label: string; value: string; chip?: React.ReactNode; link: string; onLink: () => void; children?: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1.5 border-(--line-soft) border-b px-3 py-2.5 last:border-b-0">
      <div className="flex items-center gap-2">
        <Icon className="size-3.5 shrink-0 text-(--fg-dim)" />
        <span className="w-16 shrink-0 font-bold text-(--fg-dim) text-[10px] tracking-[.08em]">{label}</span>
        <span className="min-w-0 flex-1 truncate text-(--fg)">{value}</span>
        {chip}
        <button className="shrink-0 text-(--link) text-[10.5px] hover:underline" onClick={onLink} type="button">
          {link}
        </button>
      </div>
      {children}
    </div>
  );
}

const Bar = ({ fill, hue }: { fill: number; hue: string }) => (
  <span className="h-1.5 overflow-hidden rounded-full bg-(--bg-chip)">
    <span className="block h-full rounded-full" style={{ width: `${Math.max(0, Math.min(100, fill))}%`, background: hue }} />
  </span>
);

export function ResourcePill() {
  const meters = useCached<Meters>("mac:meters", () => macMeters());
  const { reload } = meters;
  usePoll(useCallback(() => void reload(), [reload]), 15_000);
  useEffect(() => {
    // A level changed: the meters it carries, the history kept.
    const offMeters = onEnvelope("mac-meters", (e) =>
      patchKept<Meters>("mac:meters", (r) => ({ ...r, ...(e as unknown as Partial<Meters>), history: r.history })),
    );
    // The disk report the engine room and the Disk cards keep, current too.
    const offDisk = onEnvelope("disk-low", (e) =>
      patchKept<DiskReport>("disk", (r) => ({
        ...r,
        low: e.low === true,
        free: typeof e.free === "number" ? e.free : r.free,
        total: typeof e.total === "number" ? e.total : r.total,
      })),
    );
    return () => {
      offMeters();
      offDisk();
    };
  }, []);
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));

  const m = meters.data;
  if (!m) return null;
  const all = metersOf(m);
  const worst = all.reduce<Meter | null>((w, x) => (RANK[x.level] > RANK[w?.level ?? "ok"] ? x : w), null);
  const tone = worst ? (worst.level === "bad" ? "border-(--bad-border) bg-(--bad-bg)" : "border-(--warn-border) bg-(--warn-bg)") : "border-(--line) bg-(image:--grad-button)";
  const go = (part: "disk" | "mac") => {
    setOpen(false);
    openEngineRoomOn(part);
  };
  const memPct = m.memory.total > 0 ? (m.memory.used / m.memory.total) * 100 : 0;

  return (
    <div className="relative mr-2" ref={box}>
      <button
        aria-expanded={open}
        aria-label="This Mac"
        className={`flex h-6 items-center gap-2.5 rounded-full border px-2.5 text-(--fg-body) text-[11px] ${tone}`}
        data-testid="resource-pill"
        onClick={() => setOpen((o) => !o)}
        title="This Mac"
        type="button"
      >
        {all.map((x) =>
          x === worst ? (
            <span className="flex items-center gap-1 font-bold" key={x.id} style={{ color: HUE[x.level] }}>
              <x.icon className="size-3.5 shrink-0" />
              <span className="truncate">{x.say}</span>
            </span>
          ) : (
            <span className="flex items-center gap-1" key={x.id} title={`${x.label}: ${x.say}`}>
              <x.icon className="size-3.5 shrink-0 text-(--fg-dim)" />
              <span className="h-1.5 w-4 overflow-hidden rounded-full bg-(--bg-chip)">
                <span className="block h-full rounded-full" style={{ width: `${Math.max(8, Math.min(100, x.fill))}%`, background: HUE[x.level] }} />
              </span>
            </span>
          ),
        )}
      </button>
      {open && (
        <div aria-label="This Mac" className={`${POP} w-[440px] overflow-hidden p-0`} role="dialog">
          <div className="flex h-9 items-center gap-2 border-(--line) border-b bg-(--bg-chip) px-3 font-bold text-(--fg) text-[10.5px] tracking-[.08em]">THIS MAC</div>
          <Row
            chip={m.disk.level !== "ok" ? <Chip tone={m.disk.level === "bad" ? "bad" : "warn"}>{m.disk.level === "bad" ? "LOW" : "GETTING LOW"}</Chip> : undefined}
            icon={HardDriveIcon}
            label="DISK"
            link="Disk ›"
            onLink={() => go("disk")}
            value={`${bytes(m.disk.free)} free of ${bytes(m.disk.total)}`}
          >
            <Bar fill={m.disk.total > 0 ? ((m.disk.total - m.disk.free) / m.disk.total) * 100 : 0} hue={HUE[m.disk.level]} />
          </Row>
          <Row
            chip={m.memory.level !== "ok" ? <Chip tone={m.memory.level === "bad" ? "bad" : "warn"}>PRESSURE {m.memory.pressure.toUpperCase()}</Chip> : undefined}
            icon={MemoryStickIcon}
            label="MEMORY"
            link="This Mac ›"
            onLink={() => go("mac")}
            value={`${bytes(m.memory.used)} of ${bytes(m.memory.total)} · pressure ${m.memory.pressure}${m.memory.swap_used ? ` · swap ${bytes(m.memory.swap_used)}` : ""}`}
          >
            <Bar fill={memPct} hue={HUE[m.memory.level]} />
          </Row>
          <Row
            chip={m.cpu.level !== "ok" ? <Chip tone={m.cpu.level === "bad" ? "bad" : "warn"}>BUSY</Chip> : undefined}
            icon={CpuIcon}
            label="CPU"
            link="This Mac ›"
            onLink={() => go("mac")}
            value={`${Math.round(m.cpu.percent)}% · load ${m.cpu.load1.toFixed(1)} on ${m.cpu.cores} cores`}
          >
            <Spark points={m.history.map((s) => s.cpu_percent)} />
          </Row>
          <Row
            chip={m.heat.holding ? <Chip tone="bad">NEW TURNS WAIT</Chip> : m.heat.level !== "ok" ? <Chip tone={m.heat.level === "bad" ? "bad" : "warn"}>{(THERMAL[m.heat.thermal ?? ""]?.[1] ?? "hot").toUpperCase()}</Chip> : undefined}
            icon={ThermometerIcon}
            label="HEAT"
            link="This Mac ›"
            onLink={() => go("mac")}
            value={`${THERMAL[m.heat.thermal ?? ""]?.[1] ?? m.heat.thermal ?? "…"}${typeof m.heat.hottest_c === "number" ? ` · hottest sensor ${Math.round(m.heat.hottest_c)} °C` : ""}`}
          />
        </div>
      )}
    </div>
  );
}
