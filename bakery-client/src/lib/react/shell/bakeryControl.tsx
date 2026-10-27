// What the Bakery card and the top bar's Hot reload and Deploy share: the
// node's deployed-build status (read every 1.5 s while an action runs, else
// now and then), the actions that start from it (each asked first in
// Versions' words where it interrupts anything), and how the last one reads:
// a spinner while it runs, its verdict once it ends, and the log's tail while
// it runs or after it failed.

import { useCallback, useState } from "react";
import { type ControlStatus, controlBakery, controlBakerySet, type DeployedBuild, type LastAction } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago } from "$lib/conversations";
import { Chip, said, usePoll } from "./ui";

/** What the buttons start. `deploy-ui` is Hot reload: only the UI is built
 *  and swapped in, the open app reloads, nothing restarts. */
export type Action = "deploy" | "deploy-ui" | "restart" | "rollback";

const INTERRUPTS = "Turns in flight are interrupted; Resume picks them up.";
const DOING: Record<string, string> = {
  deploy: "Deploying",
  "deploy-ui": "Hot reloading",
  restart: "Restarting",
  rollback: "Rolling back",
  "upgrade-adapter": "Upgrading the Claude adapter",
};
const DID: Record<string, string> = {
  deploy: "Deploy",
  "deploy-ui": "Hot reload",
  restart: "Restart",
  rollback: "Roll back",
  "upgrade-adapter": "Claude adapter upgrade",
};

export const when = (ts: string) => (ago(ts) === "now" ? "just now" : `${ago(ts)} ago`);
export const build = (b?: DeployedBuild | null) =>
  b?.commit ? `${b.commit.slice(0, 7)}${b.built_at ? ` · built ${when(b.built_at)}` : ""}` : "the build before";

/** What the operator is asked first, or null for an action that interrupts
 *  nothing (Hot reload: no restart, no turn touched). */
function confirmText(action: Action, c: ControlStatus | null): string | null {
  if (action === "deploy-ui") return null;
  if (action === "deploy")
    return `Deploy builds the node and the app from the current source and switches to them only if both builds pass. Bakery then restarts on the new build and replaces this window. ${INTERRUPTS} Deploy now?`;
  if (action === "restart")
    return c?.deployed
      ? `Restart the node on the deployed build? The app reconnects by itself. ${INTERRUPTS}`
      : `Nothing is deployed yet, so Restart rebuilds the node from source first. The app reconnects by itself. ${INTERRUPTS} Restart?`;
  return `Roll back to ${build(c?.previous)}? Bakery restarts on it and replaces this window. ${INTERRUPTS}`;
}

/** The node's status of the deployed build, kept current, and `run`, which
 *  starts an action once the operator agrees to it. */
export function useBakeryControl() {
  const [c, setC] = useState<ControlStatus | null>(null);
  /** Why the last start was refused. */
  const [error, setError] = useState("");
  /** Why the last read failed (the node is restarting, say). */
  const [offline, setOffline] = useState("");
  const load = useCallback(
    () =>
      controlBakery().then(
        (s) => {
          setC(s);
          setError("");
          setOffline("");
        },
        (e) => setOffline(said(e)),
      ),
    [],
  );
  const last = c?.last ?? null;
  usePoll(useCallback(() => void load(), [load]), last?.running ? 1500 : 30_000);
  const run = async (action: Action) => {
    const text = confirmText(action, c);
    if (text && !(await ask(text))) return;
    setError("");
    controlBakerySet({ action }).then(setC, (e) => setError(said(e)));
  };
  return {
    c,
    last,
    error,
    offline,
    /** Source files changed since the deployed build began. */
    undeployed: c?.undeployed_count ?? 0,
    /** Nothing read yet, or an action is running: none starts now. */
    busy: !c || !!last?.running,
    run,
  };
}

/** The last action: a spinner while it runs, DONE, FAILED or ENDED once it
 *  has, and the tail of its log while it runs or after it failed. */
export function LastRun({ last }: { last: LastAction }) {
  return (
    <div className="flex flex-col gap-1 rounded-[6px] border border-(--line-soft) bg-(--bg-app) px-2 py-1.5">
      <div className="flex min-w-0 items-center gap-2">
        {last.running ? (
          <span className="size-3 shrink-0 animate-spin rounded-full border-2 border-(--line) border-t-(--accent)" />
        ) : (
          <Chip tone={last.ok === false ? "bad" : last.ok ? "ok" : "neutral"}>{last.ok === false ? "FAILED" : last.ok ? "DONE" : "ENDED"}</Chip>
        )}
        <span className="min-w-0 flex-1 truncate text-(--fg-body)">{last.running ? `${DOING[last.action] ?? last.action}…` : (DID[last.action] ?? last.action)}</span>
        <span className="shrink-0 text-(--fg-faint) text-[10.5px]">{when(last.started_at)}</span>
      </div>
      {(last.running || last.ok === false) && last.log_tail.length > 0 && (
        <pre className="max-h-28 overflow-auto whitespace-pre-wrap break-all text-(--fg-2) text-[10.5px] leading-snug">{last.log_tail.slice(-8).join("\n")}</pre>
      )}
    </div>
  );
}
