// The doctor, as a control-plane card: each thread's toolchains — what its
// folder pins (mise's tools, Rust, FVM's Flutter, Xcode), from which file,
// beside what its own room runs — ready, or why not, in the tool's words.
// Each thread's answer is asked inside its room, so it's kept and asked
// again only after a while, or on Reload.

import { useState } from "react";
import { StethoscopeIcon } from "lucide-react";
import { threadToolchains, type Toolchains } from "$lib/api.generated";
import type { Conversation } from "$lib/conversations";
import { useCached } from "./cache";
import { Card } from "./ControlCards";
import { Chip, Freshness } from "./ui";

const TEN_MINUTES = 10 * 60_000;

function ThreadToolchains({ thread }: { thread: Conversation }) {
  const { data, error, busy, at, reload } = useCached<Toolchains>(`toolchains:${thread.streamId}`, () => threadToolchains({ stream_id: thread.streamId }), {
    maxAge: TEN_MINUTES,
  });
  const [open, setOpen] = useState(false);
  const all = data?.toolchains ?? [];
  const notReady = all.filter((t) => !t.ready);
  const shown = open ? all : notReady;
  return (
    <div className="flex flex-col gap-0.5" data-testid="thread-toolchains">
      <div className="flex min-h-7 items-center gap-2">
        <button className="min-w-0 flex-1 truncate text-left text-(--fg)" onClick={() => setOpen((o) => !o)} title={data?.folder} type="button">
          {thread.title}
        </button>
        {data &&
          (all.length === 0 ? (
            <span className="text-(--fg-dim) text-[10.5px]">pins none</span>
          ) : notReady.length === 0 ? (
            <Chip tone="ok">ALL {all.length} READY</Chip>
          ) : (
            <Chip tone="bad">{notReady.length} NOT READY</Chip>
          ))}
        <Freshness at={at} busy={busy} error={error} onReload={() => void reload()} />
      </div>
      {shown.map((t) => (
        <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-0.5 pl-3 text-[11px]" key={`${t.manager}:${t.tool}`}>
          <span className={`size-1.5 shrink-0 rounded-full ${t.ready ? "bg-(--ok)" : "bg-(--bad)"}`} />
          <span className="font-mono text-(--fg)">{t.tool}</span>
          <span className="text-(--fg-dim)">
            {t.wanted ? `wants ${t.wanted}` : "pins none"}
            {t.source ? ` (${t.source})` : ""} · {t.found ? `runs ${t.found}` : "runs none"} · {t.manager}
          </span>
          {!t.ready && t.why && (
            <span className="min-w-0 basis-full truncate pl-3.5 text-(--bad)" title={t.why}>
              {t.why}
            </span>
          )}
        </div>
      ))}
    </div>
  );
}

/** Every thread of the project, its toolchains' readiness at a glance; a
 *  thread's name shows all of them, ready ones too. */
export function ToolchainsCard({ threads }: { threads: Conversation[] }) {
  if (threads.length === 0) return null;
  return (
    <Card hue="var(--ok)" icon={<StethoscopeIcon className="size-3.5" />} title="Toolchains" wide>
      <div className="flex flex-col gap-1 p-1" data-testid="toolchains">
        {threads.map((t) => (
          <ThreadToolchains key={t.streamId} thread={t} />
        ))}
      </div>
    </Card>
  );
}
