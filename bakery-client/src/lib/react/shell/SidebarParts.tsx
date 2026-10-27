// Two parts of the shell's sidebar: the node box at its top left (this
// machine, the nodes and phones paired with it, reaching it from them, and
// pairing a phone) and the small form that cuts a new worktree.

import { useEffect, useRef, useState, type ReactNode } from "react";
import { MonitorIcon, ServerIcon, SmartphoneIcon } from "lucide-react";
import {
  devices,
  devicesPairCode,
  devicesRemote,
  devicesRevoke,
  type DeviceView,
  type Devices,
} from "$lib/api.generated";
import { ask } from "$lib/ask";
import { onStatus, type ConnStatus } from "$lib/events";
import { node, type NodeInfo } from "$lib/node";
import { keep, useCached } from "./cache";
import { said } from "./ui";

/** The connection's dot: a halo in the colour of its state. */
const DOT: Record<ConnStatus, string> = {
  open: "bg-(--ok) shadow-[0_0_0_2px_color-mix(in_srgb,var(--ok)_25%,transparent)]",
  connecting: "bg-(--warn) shadow-[0_0_0_2px_color-mix(in_srgb,var(--warn)_25%,transparent)] motion-safe:animate-pulse",
  closed: "bg-(--bad) shadow-[0_0_0_2px_color-mix(in_srgb,var(--bad)_25%,transparent)]",
};

const FALLBACK: Record<string, string> = {
  local_only: "on this machine only",
  prefer_local: "here first, a server node when this one is away",
  prefer_server: "on a server node first",
  auto: "where the node decides",
};

function ago(iso: string | null | undefined): string {
  if (!iso) return "";
  const mins = Math.round((Date.now() - Date.parse(iso)) / 60_000);
  if (!Number.isFinite(mins)) return "";
  if (mins < 1) return "seen just now";
  return mins < 60 ? `seen ${mins}m ago` : `seen ${Math.round(mins / 60)}h ago`;
}

function Group({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="space-y-0.5">
      <p className="px-0.5 font-semibold text-(--fg-dim) text-[10.5px] uppercase tracking-[.08em]">{label}</p>
      {children}
    </div>
  );
}

/** A one-time code a phone pairs with: asked of the node, counting down
 *  its five minutes. Shared by the node box and quick settings. */
export function usePairCode() {
  const [code, setCode] = useState<{ code: string; url: string; until: number } | null>(null);
  const [problem, setProblem] = useState("");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!code) return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [code]);
  async function pair() {
    setProblem("");
    try {
      const c = await devicesPairCode();
      setCode({ code: c.code, url: c.url, until: Date.now() + c.expires_in_secs * 1000 });
      setNow(Date.now());
    } catch (e) {
      // 409 while the node isn't reachable: the reason says what to turn on.
      setCode(null);
      setProblem(said(e));
    }
  }
  const left = code ? Math.max(0, Math.round((code.until - now) / 1000)) : 0;
  return { code, left, problem, pair };
}

/** The code, while it lasts, and where a phone enters it. */
export function PairCodeShown({ code, left }: { code: { code: string; url: string } | null; left: number }) {
  if (!code) return null;
  if (left === 0) return <p className="text-(--fg-dim) text-[11px]">Expired</p>;
  return (
    <div className="space-y-1">
      <p aria-live="polite" className="font-semibold text-(--fg) text-[22px] tracking-[.06em]">
        {code.code}
      </p>
      <p className="text-(--fg-dim) text-[11px]">
        {Math.floor(left / 60)}:{String(left % 60).padStart(2, "0")}
      </p>
      <p className="break-all text-(--fg-2) text-[11px]">{code.url}</p>
    </div>
  );
}

/** The paired nodes and phones, reaching this node from them, and pairing a
 *  phone. Shared by the node box and Settings › Machines. */
export function DevicesView({ devs, onChange }: { devs: Devices; onChange: (d: Devices) => void }) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const pairing = usePairCode();

  async function revoke(d: DeviceView) {
    if (!(await ask(`Revoke ${d.name}? Its connections close at once, and it has to pair again.`))) return;
    try {
      onChange(await devicesRevoke({ device_id: d.device_id }));
    } catch (e) {
      setProblem(said(e));
    }
  }
  async function setRemote(enabled: boolean) {
    setBusy(true);
    setProblem("");
    try {
      onChange({ ...devs, remote: await devicesRemote({ enabled }) });
    } catch (e) {
      setProblem(said(e));
    } finally {
      setBusy(false);
    }
  }
  const row = (d: DeviceView) => (
    <div className="flex h-[30px] items-center gap-2 rounded-md px-2" key={d.device_id}>
      {d.kind === "phone" ? (
        <SmartphoneIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
      ) : (
        <ServerIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
      )}
      <span className="min-w-0 flex-1 truncate text-(--fg-body)">{d.name}</span>
      <span className="shrink-0 text-(--fg-dim) text-[11px]" title={`Paired ${new Date(d.paired_at).toLocaleString()}`}>
        {ago(d.last_seen)}
      </span>
      <button
        aria-label={`Revoke ${d.name}`}
        className="h-[22px] shrink-0 rounded-[5px] border border-(--bad-border) px-1.5 text-(--bad) text-[11px] hover:bg-(--bad-bg)"
        onClick={() => void revoke(d)}
        type="button"
      >
        Revoke
      </button>
    </div>
  );
  const nodes = devs.devices.filter((d) => d.kind === "node");
  const phones = devs.devices.filter((d) => d.kind === "phone");

  return (
    <>
      {nodes.length > 0 && <Group label="Nodes">{nodes.map(row)}</Group>}
      {phones.length > 0 && <Group label="Phones">{phones.map(row)}</Group>}
      <div className="h-px bg-(--line-chrome)" />
      <div className="space-y-1">
        <div className="flex items-center gap-2.5">
          <button
            aria-checked={devs.remote.enabled}
            aria-label="Reachable from your other devices"
            className={`relative h-[18px] w-8 shrink-0 rounded-full disabled:opacity-50 ${devs.remote.enabled ? "bg-(--accent-solid)" : "bg-(--bg-chip)"}`}
            disabled={busy}
            onClick={() => void setRemote(!devs.remote.enabled)}
            role="switch"
            type="button"
          >
            <span
              className={`absolute top-0.5 size-3.5 rounded-full bg-(--bg-app) ${devs.remote.enabled ? "left-4" : "left-0.5"}`}
            />
          </button>
          <span className="text-(--fg)">Reachable from your other devices</span>
        </div>
        {devs.remote.enabled && devs.remote.problem && (
          <p className="ml-[42px] text-(--warn) text-[11px]">{devs.remote.problem}</p>
        )}
        {devs.remote.enabled && !devs.remote.problem && devs.remote.url && (
          <p className="ml-[42px] break-all text-(--ok) text-[11px]">{devs.remote.url}</p>
        )}
      </div>
      <div className="h-px bg-(--line-chrome)" />
      <div className="space-y-1.5">
        <div className="flex items-center gap-2">
          <span className="font-semibold text-(--fg)">Pair a phone</span>
          <span className="flex-1" />
          <button
            className="h-6 rounded-md border border-(--line-strong) px-2.5 text-(--fg-body) text-[11px] hover:bg-(--bg-chip)"
            onClick={() => void pairing.pair()}
            type="button"
          >
            {pairing.code ? "New code" : "Get a code"}
          </button>
        </div>
        <PairCodeShown code={pairing.code} left={pairing.left} />
        {(problem || pairing.problem) && <p className="text-(--warn) text-[11px]">{problem || pairing.problem}</p>}
      </div>
    </>
  );
}

function ThisMac({ name, word, info }: { name: string; word: string; info: NodeInfo | null }) {
  return (
    <Group label="This Mac">
      <div className="flex h-[30px] items-center gap-2 rounded-md bg-(--accent-sel) px-2">
        <MonitorIcon className="size-3.5 shrink-0 text-(--link)" />
        <span className="min-w-0 flex-1 truncate text-(--fg)">{name}</span>
        <span className="shrink-0 text-(--fg-dim) text-[11px]">{word}</span>
      </div>
      {info && (
        <p className="flex gap-2 px-2 text-[11px]">
          <span className="shrink-0 whitespace-nowrap text-(--fg-dim)">New work</span>
          <span className="text-(--fg-2)">{FALLBACK[info.stream_fallback] ?? info.stream_fallback}</span>
        </p>
      )}
    </Group>
  );
}

/** This Mac's node, as last read (kept across restarts), read again on open. */
export const useNodeInfo = () => useCached<NodeInfo>("node:info", () => node.info());
/** The machines and phones, likewise. */
export const useDevices = () => useCached<Devices>("devices", () => devices());
const keepDevices = (d: Devices) => keep("devices", d);

/** Settings › Machines: the node box's list, as a page. */
export function MachinesSettings() {
  const { data: info } = useNodeInfo();
  const { data: devs, error } = useDevices();
  return (
    <div className="bakery-ai max-w-[520px] space-y-3 text-(--fg-body) text-xs">
      <ThisMac info={info} name={info?.hostname || "This Mac"} word={info ? "online" : "…"} />
      {devs ? (
        <DevicesView devs={devs} onChange={keepDevices} />
      ) : error ? (
        <p className="text-(--bad) text-[11px]">{error}</p>
      ) : (
        <p className="text-(--fg-dim) text-[11px]">Loading…</p>
      )}
    </div>
  );
}

/** This machine at the sidebar's top left. Opens the machines, nodes and
 *  phones list, with pairing. */
export function NodeBox({ onSettings, onEngine }: { onSettings: () => void; onEngine: () => void }) {
  const [status, setStatus] = useState<ConnStatus>("connecting");
  const { data: info, reload: reloadInfo } = useNodeInfo();
  const { data: devs, error, reload: reloadDevices } = useDevices();
  const setDevs = keepDevices;
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => onStatus(setStatus), []);
  useEffect(() => {
    if (status !== "open") return;
    void reloadInfo();
    void reloadDevices();
  }, [status, reloadInfo, reloadDevices]);
  useEffect(() => {
    if (open) void reloadDevices();
  }, [open, reloadDevices]);
  useEffect(() => {
    if (!open) return;
    const away = (e: Event) => {
      if (e instanceof KeyboardEvent ? e.key === "Escape" : !box.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", away);
    document.addEventListener("keydown", away);
    return () => {
      document.removeEventListener("pointerdown", away);
      document.removeEventListener("keydown", away);
    };
  }, [open]);

  const word = status === "open" ? "online" : status === "connecting" ? "connecting…" : "disconnected";
  const name = info?.hostname || "This Mac";
  const count = (kind: DeviceView["kind"]) => devs?.devices.filter((d) => d.kind === kind).length ?? 0;
  const phones = count("phone");
  const nodes = count("node");
  const down = status === "closed";
  return (
    <div className="relative min-w-0 self-start" ref={box}>
      {/* Only the device and its dot: online is green (the operator, Round
          10), connecting amber, and a node that does not answer red, the
          whole button with it. What it is and who is paired is in the popover. */}
      <button
        aria-expanded={open}
        aria-haspopup="dialog"
        aria-label={`Machines · ${name} · ${word}`}
        className={`flex h-[30px] items-center gap-2 rounded-md border px-[9px] ${open ? "border-(--accent-border) bg-(--bg-chip)" : down ? "border-(--bad-border) bg-(--bad-bg)" : "border-(--line) bg-(image:--grad-button) hover:border-(--line-strong)"}`}
        data-testid="shell-machines"
        onClick={() => setOpen((v) => !v)}
        title={`Machines · ${name} · ${word}`}
        type="button"
      >
        <MonitorIcon className={`size-3.5 shrink-0 ${down ? "text-(--bad)" : "text-(--link)"}`} />
        <span aria-hidden className={`size-2 shrink-0 rounded-full ${DOT[status]}`} />
      </button>
      {open && (
        <div
          aria-label="Machines and devices"
          className="absolute top-full left-0 z-30 mt-1.5 w-[380px] max-w-[calc(100vw-24px)] space-y-3 rounded-[10px] border border-(--line-strong) bg-(--bg-raised) p-3 text-xs shadow-(--shadow-menu)"
          role="dialog"
        >
          <div className="flex items-center gap-2">
            <h2 className="font-semibold text-(--fg) text-xs">Machines</h2>
            {nodes > 0 && (
              <span className="flex shrink-0 items-center gap-[3px] text-(--violet) text-[11px]" title="Nodes">
                <ServerIcon className="size-[11px]" />
                {nodes}
              </span>
            )}
            {phones > 0 && (
              <span className="flex shrink-0 items-center gap-[3px] text-(--violet) text-[11px]" title="Phones">
                <SmartphoneIcon className="size-[11px]" />
                {phones}
              </span>
            )}
            <span className="flex-1" />
            <button
              className="text-(--accent-fg) text-[11px] hover:text-(--fg)"
              onClick={() => {
                setOpen(false);
                onEngine();
              }}
              type="button"
            >
              Machines panel ›
            </button>
            <button
              className="text-(--accent-fg) text-[11px] hover:text-(--fg)"
              onClick={() => {
                setOpen(false);
                onSettings();
              }}
              type="button"
            >
              Settings › Machines
            </button>
          </div>
          <ThisMac info={info} name={name} word={word} />
          {devs ? (
            <DevicesView devs={devs} onChange={setDevs} />
          ) : error ? (
            <p className="text-(--bad) text-[11px]">{error}</p>
          ) : (
            <p className="text-(--fg-dim) text-[11px]">Loading…</p>
          )}
        </div>
      )}
    </div>
  );
}

/** Names a new worktree (also its branch); `create` cuts it with its agent. */
export function NewWorktreeForm({ create, onDone }: { create: (name: string) => Promise<void>; onDone: () => void }) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  return (
    <form
      className="space-y-1 px-1.5 py-1"
      onSubmit={(e) => {
        e.preventDefault();
        const n = name.trim();
        if (!n) return;
        setBusy(true);
        setError("");
        create(n).then(onDone, (err) => {
          setError(said(err));
          setBusy(false);
        });
      }}
    >
      <input
        aria-label="New worktree name"
        autoFocus
        className="w-full rounded-md border border-(--line) bg-(--bg-app) px-1.5 py-0.5 text-xs outline-none focus:border-(--accent-border)"
        disabled={busy}
        onBlur={() => !name.trim() && onDone()}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && onDone()}
        placeholder="name, e.g. login-fix (also its branch)"
        value={name}
      />
      {error && <p className="text-(--bad) text-[11px]">{error}</p>}
    </form>
  );
}
