// Quick settings (Round 7, Q4: what you change most), opened by the gear at
// the sidebar's bottom left: the look (Warm, Auto or Light, and the bakery
// themes), this Mac with its phones and a pairing code, and All settings
// (the popout, ⌘,). How new threads start and the needs-you alerts join
// once the node has them.

import { useEffect, useMemo, useState } from "react";
import { MonitorIcon, SettingsIcon, SmartphoneIcon, SparklesIcon } from "lucide-react";
import { BAKERY_THEMES, type BakeryTheme } from "$lib/bakeryThemes";
import { onStatus, type ConnStatus } from "$lib/events";
import { applyTheme, loadAppearance, saveScheme, THEME_CHANGED_EVENT, type Theme } from "$lib/theme";
import { parseTweakcn, tweakcnScheme } from "$lib/tweakcn";
import { PairCodeShown, useDevices, useNodeInfo, usePairCode } from "./SidebarParts";
import { Chip, said } from "./ui";

/** The looks the scheme switch names: Warm and Parchment are look C's dark
 *  and light; Auto follows the Mac between them. */
const SCHEMES: [Theme, string][] = [
  ["warm", "Warm"],
  ["auto", "Auto"],
  ["parchment", "Light"],
];
const SECTION = "flex flex-col gap-2 border-(--line-soft) border-b px-3 py-2.5";
const LABEL = "flex items-center gap-1.5 font-bold text-[10px] tracking-[.08em]";

/** A bakery theme's two colours, its ground and its accent, for a swatch. */
function swatchOf(t: BakeryTheme): [string, string] {
  const parsed = parseTweakcn(t.css);
  const v = { ...parsed.theme, ...(parsed.dark ?? parsed.light ?? {}) };
  return [v.background ?? "#222", v.primary ?? "#888"];
}

export function QuickSettings({ onAll }: { onAll: () => void }) {
  const [appearance, setAppearance] = useState(loadAppearance);
  useEffect(() => {
    const again = () => setAppearance(loadAppearance());
    window.addEventListener(THEME_CHANGED_EVENT, again);
    return () => window.removeEventListener(THEME_CHANGED_EVENT, again);
  }, []);
  const [problem, setProblem] = useState("");
  const swatches = useMemo(() => BAKERY_THEMES.map((t) => ({ t, colors: swatchOf(t) })), []);
  /** A bakery theme, imported the first time it's picked, then applied. */
  const pickTheme = (t: BakeryTheme) => {
    setProblem("");
    try {
      const saved = loadAppearance().schemes.find((s) => s.name === t.name);
      if (saved) applyTheme(saved.id);
      else saveScheme(tweakcnScheme(parseTweakcn(t.css), t.name));
    } catch (e) {
      setProblem(said(e));
    }
    setAppearance(loadAppearance());
  };
  const selectedName = appearance.schemes.find((s) => s.id === appearance.selected)?.name ?? null;

  return (
    <div
      aria-label="Quick settings"
      className="w-80 overflow-hidden rounded-[12px] border border-(--line-strong) bg-(--bg-raised) text-xs shadow-(--shadow-menu)"
      role="dialog"
    >
      <div className="flex h-9 items-center gap-2 border-(--line-soft) border-b bg-(image:--grad-button) px-3 font-bold text-(--fg)">
        <SettingsIcon className="size-3.5 text-(--accent)" />
        Quick settings
      </div>
      <div className={SECTION}>
        <div className={`${LABEL} text-(--link)`}>
          <SparklesIcon className="size-3" />
          LOOK
        </div>
        <div className="flex items-center gap-2">
          <span className="flex-1 text-(--fg-body)">Scheme</span>
          <span className="flex overflow-hidden rounded-[5px] border border-(--line)">
            {SCHEMES.map(([id, label]) => (
              <button
                aria-pressed={appearance.selected === id}
                className={`h-6 px-2.5 text-[11px] ${appearance.selected === id ? "bg-(--bg-pressed) font-bold text-(--accent-fg)" : "text-(--fg-2) hover:text-(--fg)"}`}
                key={id}
                onClick={() => {
                  applyTheme(id);
                  setAppearance(loadAppearance());
                }}
                type="button"
              >
                {label}
              </button>
            ))}
          </span>
        </div>
        <div className="flex flex-wrap gap-1.5">
          {swatches.map(({ t, colors }) => {
            const on = selectedName === t.name;
            return (
              <button
                aria-label={t.name}
                aria-pressed={on}
                className={`flex size-7 overflow-hidden rounded-lg border ${on ? "border-(--accent) shadow-[0_0_0_2px_var(--accent-border)]" : "border-(--line) hover:border-(--line-strong)"}`}
                key={t.name}
                onClick={() => pickTheme(t)}
                title={`${t.name}: ${t.blurb}`}
                type="button"
              >
                <span className="flex-1" style={{ background: colors[0] }} />
                <span className="flex-1" style={{ background: colors[1] }} />
              </button>
            );
          })}
        </div>
        {problem && <p className="text-(--bad) text-[11px]">{problem}</p>}
      </div>
      <ThisMac />
      <button className="flex h-9 w-full items-center gap-2 px-3 text-left font-bold text-(--accent-fg) hover:bg-(--bg-chip)" onClick={onAll} type="button">
        <SettingsIcon className="size-3.5" />
        <span className="flex-1">All settings</span>
        <span className="font-normal text-(--fg-dim)">⌘,</span>
      </button>
    </div>
  );
}

/** This Mac: its node, the phones paired with it, and a code to pair one. */
function ThisMac() {
  const [status, setStatus] = useState<ConnStatus>("connecting");
  const { data: info, reload: reloadInfo } = useNodeInfo();
  const { data: devs, reload: reloadDevices } = useDevices();
  const pairing = usePairCode();
  useEffect(() => onStatus(setStatus), []);
  useEffect(() => {
    if (status !== "open") return;
    void reloadInfo();
    void reloadDevices();
  }, [status, reloadInfo, reloadDevices]);
  const phones = devs?.devices.filter((d) => d.kind === "phone") ?? [];
  const tone = status === "open" ? "ok" : status === "connecting" ? "warn" : "bad";
  const word = status === "open" ? "ONLINE" : status === "connecting" ? "CONNECTING" : "OFFLINE";
  return (
    <div className={SECTION}>
      <div className={`${LABEL} text-(--ok)`}>
        <MonitorIcon className="size-3" />
        THIS MAC
      </div>
      <div className="flex items-center gap-2">
        <span className="min-w-0 flex-1 truncate text-(--fg)">{info?.hostname || "This Mac"}</span>
        <Chip tone={tone}>{word}</Chip>
      </div>
      <div className="flex items-center gap-2">
        <SmartphoneIcon className="size-3.5 shrink-0 text-(--fg-dim)" />
        <span className="min-w-0 flex-1 truncate text-(--fg-body)" title={phones.map((p) => p.name).join(", ")}>
          {phones.length === 0 ? "No phones yet" : `${phones.length} ${phones.length === 1 ? "phone" : "phones"} · ${phones.map((p) => p.name).join(", ")}`}
        </span>
        <button
          className="h-6 shrink-0 rounded-[5px] border border-(--line) bg-(image:--grad-button) px-2.5 text-(--fg-body) text-[11px] hover:text-(--fg)"
          onClick={() => void pairing.pair()}
          type="button"
        >
          {pairing.code ? "New code" : "Pair"}
        </button>
      </div>
      <PairCodeShown code={pairing.code} left={pairing.left} />
      {pairing.problem && <p className="text-(--warn) text-[11px]">{pairing.problem}</p>}
    </div>
  );
}
