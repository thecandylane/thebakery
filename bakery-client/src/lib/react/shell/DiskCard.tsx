// A project's part of the disk, in its control plane: its threads' build
// folders (Cargo's, Flutter's, npm's…), largest first, each cleared only on
// the operator's yes, and the disk's free space. The whole disk is the
// engine room's Disk.

import { useState } from "react";
import { HardDriveIcon } from "lucide-react";
import { diskClean } from "$lib/api.generated";
import { ask } from "$lib/ask";
import type { Conversation } from "$lib/conversations";
import { Card } from "./ControlCards";
import { bytes } from "./containersModel";
import { useDisk } from "./EngineRoom";
import { PLAIN } from "./Internet";
import { Chip, Freshness, said } from "./ui";

export function DiskCard({ threads }: { threads: Conversation[] }) {
  const d = useDisk();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const ids = new Set(threads.map((t) => t.streamId));
  const folders = (d.data?.build_folders ?? []).filter((f) => ids.has(f.stream_id));
  if (!d.data || (folders.length === 0 && !d.data.low)) return null;
  const total = folders.reduce((n, f) => n + f.bytes, 0);
  const clean = async (path: string, question: string) => {
    if (!(await ask(question))) return;
    setBusy(true);
    setError("");
    diskClean({ path, what: "build_folder" }).then(
      () => {
        setBusy(false);
        void d.reload();
      },
      (e) => {
        setBusy(false);
        setError(said(e));
      },
    );
  };
  return (
    <Card
      action={
        <>
          {d.data.low && <Chip tone="bad">DISK LOW</Chip>}
          <span className="font-normal normal-case tracking-normal">
            <Freshness at={d.at} busy={d.busy} error={d.error} onReload={() => void d.reload()} />
          </span>
        </>
      }
      hue={d.data.low ? "var(--bad)" : "var(--link)"}
      icon={<HardDriveIcon className="size-3.5" />}
      title="Disk"
      wide
    >
      <div className="flex flex-col gap-1 p-1 text-[11px]" data-testid="disk-card">
        <span className="text-(--fg-dim)">
          {bytes(d.data.free)} free of {bytes(d.data.total)}
          {folders.length > 0 ? ` · this project's build folders take ${bytes(total)}` : ""}
        </span>
        {folders.map((f) => (
          <div className="flex min-h-7 min-w-0 items-center gap-2" key={f.path}>
            <span className="w-40 shrink-0 truncate text-(--fg)">{f.thread}</span>
            <span className="shrink-0 text-(--fg-dim)">{f.made_by}</span>
            <span className="min-w-0 flex-1 truncate font-mono text-(--fg-faint) text-[10.5px]" title={f.path}>
              {f.path}
            </span>
            <span className="shrink-0 text-(--fg-body)">{bytes(f.bytes)}</span>
            <button
              className={PLAIN}
              disabled={busy}
              onClick={() => void clean(f.path, `Clear ${f.made_by}'s build folder in ${f.thread}? ${bytes(f.bytes)} goes; the next build makes it again.`)}
              type="button"
            >
              Clean
            </button>
          </div>
        ))}
        {error && <p className="text-(--bad)">{error}</p>}
      </div>
    </Card>
  );
}
