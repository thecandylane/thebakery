// The top bar's Hot reload and Deploy, between the resource pill and the
// waiting pill. Hot reload is the UI-only deploy: only the UI is built and
// swapped in, the open app reloads, nothing restarts and no turn is touched.
// Deploy is the full deploy (asked first), beside how many source files it
// has not taken yet. Each shows while its action runs and how it ended, from
// the status the Bakery card reads (`bakeryControl`): a failure opens its log
// under the pills and stays until the next action or until it is dismissed.

import { useEffect, useRef, useState } from "react";
import { CheckIcon, LoaderCircleIcon, RocketIcon, XIcon, ZapIcon } from "lucide-react";
import { type Action, LastRun, useBakeryControl } from "./bakeryControl";
import { PLAIN, SOLID } from "./Internet";
import { POP, useDismiss } from "./PanelMenu";
import { Chip } from "./ui";

type Look = "idle" | "running" | "done" | "failed" | "ended";

const PILLS = [
  { action: "deploy-ui", name: "Hot reload", ing: "Reloading", done: "Reloaded", hint: "Build only the UI; the open app reloads", testid: "hot-reload-pill" },
  { action: "deploy", name: "Deploy", ing: "Deploying", done: "Deployed", hint: "Build the node and the app, then restart Bakery on them", testid: "deploy-pill" },
] as const;

const PILL = "flex h-6 shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2.5 text-[11px] disabled:opacity-50";
const LOOK: Record<Look, string> = {
  idle: "border-(--line) bg-(image:--grad-button) text-(--fg-body) hover:border-(--line-strong) hover:text-(--fg)",
  running: "border-(--accent-border) bg-(--accent-sel) text-(--fg)",
  done: "border-(--ok-border) bg-(--ok-bg) font-bold text-(--ok)",
  failed: "border-(--bad-border) bg-(--bad-bg) font-bold text-(--bad)",
  ended: "border-(--warn-border) bg-(--warn-bg) font-bold text-(--warn)",
};
const ICON = "size-3.5 shrink-0";

/** At rest the action's own icon; otherwise how it stands. */
function PillIcon({ look, action }: { look: Look; action: Action }) {
  if (look === "running") return <LoaderCircleIcon className={`${ICON} motion-safe:animate-spin`} />;
  if (look === "done") return <CheckIcon className={ICON} />;
  if (look === "failed" || look === "ended") return <XIcon className={ICON} />;
  return action === "deploy-ui" ? <ZapIcon className={`${ICON} text-(--warn)`} /> : <RocketIcon className={`${ICON} text-(--accent)`} />;
}

export function DeployPills() {
  const { c, last, error, undeployed, busy, run } = useBakeryControl();
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useDismiss(open, box, () => setOpen(false));

  // A run seen to end shows its verdict: a good one for a few seconds, a bad
  // one (its log opened at once) until the next action or until dismissed.
  const running = useRef<string | null>(null);
  const [fresh, setFresh] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(null);
  useEffect(() => {
    if (!last) return;
    if (last.running) {
      running.current = last.started_at;
      return;
    }
    if (running.current !== last.started_at) return;
    running.current = null;
    if (last.ok) setFresh(last.started_at);
    else setOpen(true);
  }, [last]);
  useEffect(() => {
    if (!fresh) return;
    const t = setTimeout(() => setFresh(null), 6000);
    return () => clearTimeout(t);
  }, [fresh]);
  // Why a start was refused opens there too.
  useEffect(() => {
    if (error) setOpen(true);
  }, [error]);

  const look = (action: Action): Look => {
    if (last?.action !== action) return "idle";
    if (last.running) return "running";
    if (last.ok) return last.started_at === fresh ? "done" : "idle";
    if (last.started_at === dismissed) return "idle";
    return last.ok === false ? "failed" : "ended";
  };
  // The last action, when it is one of these two.
  const shown = last && PILLS.some((p) => p.action === last.action) ? last : null;
  const again = shown && !shown.running && !shown.ok && shown.started_at !== dismissed;

  return (
    <div className="relative mr-2 flex items-center gap-1.5" ref={box}>
      {PILLS.map((p) => {
        const now = look(p.action);
        const nothingDeployed = p.action === "deploy-ui" && c !== null && !c.deployed;
        const text = { idle: p.name, running: `${p.ing}…`, done: p.done, failed: `${p.name} failed`, ended: `${p.name} ended` }[now];
        // While it runs, the log's newest line; once it has started or ended,
        // the pill opens its details; at rest it says what it does.
        const title =
          now === "running"
            ? (last?.log_tail.at(-1) ?? text)
            : now !== "idle"
              ? "Details"
              : nothingDeployed
                ? "Nothing is deployed yet: Deploy first"
                : p.hint;
        return (
          <button
            aria-busy={now === "running"}
            aria-expanded={now === "idle" ? undefined : open}
            className={`${PILL} ${LOOK[now]}`}
            data-testid={p.testid}
            disabled={now === "idle" && (busy || nothingDeployed)}
            key={p.action}
            onClick={() => (now === "idle" ? void run(p.action) : setOpen((o) => !o))}
            title={title}
            type="button"
          >
            <PillIcon action={p.action} look={now} />
            {text}
            {p.action === "deploy" && now === "idle" && c && undeployed > 0 && (
              <Chip title={c.undeployed.join("\n")} tone="warn">
                {undeployed} not deployed
              </Chip>
            )}
          </button>
        );
      })}
      {open && (shown || error) && (
        <div aria-label="Hot reload and Deploy" className={`${POP} flex w-[420px] flex-col gap-1.5 p-2`} role="dialog">
          {shown && <LastRun last={shown} />}
          {error && <p className="text-(--bad) text-[11px]">{error}</p>}
          {again && (
            <div className="flex gap-1.5">
              <button
                className={SOLID}
                onClick={() => {
                  setOpen(false);
                  void run(shown.action as Action);
                }}
                type="button"
              >
                Run again
              </button>
              <button
                className={PLAIN}
                onClick={() => {
                  setDismissed(shown.started_at);
                  setOpen(false);
                }}
                type="button"
              >
                Dismiss
              </button>
            </div>
          )}
        </div>
      )}
    </div>
  );
}
