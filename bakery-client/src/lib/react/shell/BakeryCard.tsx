// The Bakery itself, on its own project's control plane (the operator's QA:
// Deploy and Restart belong there, not only in Settings › Versions): what's
// deployed and how much of the source isn't yet, what the node runs, the
// last deploy, restart or roll back with its log while it runs or once it
// fails, and the three actions, each asked first in Versions' words. The
// status and the actions are `bakeryControl`'s, which the top bar's Hot
// reload and Deploy use too.

import { RocketIcon, RotateCcwIcon, RotateCwIcon } from "lucide-react";
import { node } from "$lib/api.generated";
import { build, LastRun, useBakeryControl } from "./bakeryControl";
import { useCached } from "./cache";
import { Card } from "./ControlCards";
import { PLAIN, SOLID } from "./Internet";
import type { ShellProject } from "./shellSource.svelte";
import { Chip } from "./ui";

const LABEL = "w-[74px] shrink-0 font-bold text-(--fg-dim) text-[10px] tracking-[.08em]";

/** Shown on the Bakery's own control plane (project ""), and on the one of
 *  the project the Bakery's own source is a root of. */
export function BakeryCard({ project }: { project: ShellProject }) {
  const root = useCached("node:source", () => node()).data?.bakery_source_root ?? "";
  return !project.id || (root && project.checkouts.some((c) => c.repoRoot === root || c.path === root)) ? <Controls /> : null;
}

function Controls() {
  const { c, last, error, offline, undeployed, busy, run } = useBakeryControl();
  const problem = error || offline;
  return (
    <Card hue="var(--accent)" icon={<RocketIcon className="size-3.5" />} title="Bakery">
      <div className="flex min-w-0 items-center gap-2">
        <span className={LABEL}>DEPLOYED</span>
        <span className="min-w-0 flex-1 truncate text-(--fg)">{c ? (c.deployed ? build(c.deployed) : "nothing yet") : "…"}</span>
        {c && (
          <Chip title={c.undeployed.join("\n") || undefined} tone={!c.deployed ? "neutral" : undeployed > 0 ? "warn" : "ok"}>
            {!c.deployed ? "NOT DEPLOYED" : undeployed > 0 ? `${undeployed} NOT DEPLOYED` : "UP TO DATE"}
          </Chip>
        )}
      </div>
      <div className="flex min-w-0 items-center gap-2">
        <span className={LABEL}>NODE</span>
        <span className="min-w-0 flex-1 truncate text-(--fg-body)">{c ? (c.running_deployed ? "runs the deployed build" : "runs from source") : "…"}</span>
      </div>
      {last && <LastRun last={last} />}
      {problem && <p className="text-(--bad) text-[11px]">{problem}</p>}
      <div className="flex flex-wrap gap-1.5 pt-1">
        <button className={SOLID} disabled={busy} onClick={() => void run("deploy")} type="button">
          <RocketIcon className="mr-1 size-3" />
          Deploy
        </button>
        <button className={PLAIN} disabled={busy} onClick={() => void run("restart")} type="button">
          <RotateCwIcon className="size-3" />
          Restart
        </button>
        <button className={PLAIN} disabled={busy || !c?.previous} onClick={() => void run("rollback")} type="button">
          <RotateCcwIcon className="size-3" />
          Roll back
        </button>
      </div>
    </Card>
  );
}
