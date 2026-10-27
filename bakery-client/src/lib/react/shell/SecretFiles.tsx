// A project's secret-named files (the operator's answer: built like the
// Internet list): the ones its builds asked to read and wait on the
// operator, and the operator's answers, allowed or denied, each forgotten
// on Forget so the file asks again. Allowed files are readable, never
// writable, in every room of the project's threads. A private key takes a
// second yes. Shown once the project has any.

import { useEffect, useState } from "react";
import { KeyRoundIcon } from "lucide-react";
import { type FileReadAnswer, type FileReadAsk, filesReadAsks, filesReadDecide, type SecretFiles } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { ago } from "$lib/conversations";
import { onEvent } from "$lib/events";
import { keep, useCached } from "./cache";
import { revealInCode } from "./CodeView";
import { Card } from "./ControlCards";
import { bytes } from "./containersModel";
import { PLAIN, SOLID } from "./Internet";
import * as L from "./layout";
import type { ShellProject } from "./shellSource.svelte";
import { Chip, Freshness, said, type ToolContext } from "./ui";

const KIND_WORD: Record<string, string> = {
  public_key: "PUBLIC KEY",
  certificate: "CERTIFICATE",
  private_key: "PRIVATE KEY",
  token_or_env: "TOKEN OR ENV",
  unknown: "UNKNOWN",
};
const LABEL = "w-16 shrink-0 pt-1 font-bold text-[10px] tracking-[.06em]";

export function SecretFilesCard({ ctx, project }: { ctx: ToolContext; project: ShellProject }) {
  const key = `secret-files:${project.id}`;
  const { data, error, busy, at, reload } = useCached<SecretFiles>(key, () => filesReadAsks({ project_id: project.id }));
  const [answering, setAnswering] = useState("");
  const [answerError, setAnswerError] = useState("");
  // An ask or an answer anywhere in the project lands on its own stream.
  useEffect(
    () =>
      onEvent(project.id, (ev) => {
        if (ev.family === "file.read.ask" || ev.family === "file.read.decided") void reload();
      }),
    [project.id, reload],
  );
  const asks = data?.asks ?? [];
  const files = data?.files ?? [];
  if (asks.length === 0 && files.length === 0) return null;
  const allowed = files.filter((f) => f.allow);
  const denied = files.filter((f) => !f.allow);
  const threadOf = (sid: string) => ctx.snap.conversations.find((c) => c.streamId === sid)?.title ?? "a thread";

  const decide = async (path: string, allow: boolean | null, privateKey = false) => {
    if (allow === true && privateKey && !(await ask(`Let this project's builds read ${path}?\n\nThe node says it's a private key.`))) return;
    setAnswering(path);
    setAnswerError("");
    filesReadDecide({ project_id: project.id, path, allow, sure: allow === true && privateKey }).then(
      (next) => {
        keep(key, next);
        setAnswering("");
      },
      (e) => {
        setAnswering("");
        setAnswerError(said(e));
      },
    );
  };
  const look = (a: FileReadAsk) => {
    revealInCode(a.stream_id, a.path, 1);
    ctx.setLayout((l) => L.openPanel(l, ctx.workspace.id, `@code:${a.stream_id}`, 900));
  };
  const answer = (f: FileReadAnswer) => (
    <div className="flex min-h-7 min-w-0 items-center gap-2" key={f.path}>
      <span className="min-w-0 flex-1 truncate font-mono text-(--fg)" title={f.path}>
        {f.path}
      </span>
      <span className="shrink-0 text-(--fg-faint) text-[10.5px]">{ago(f.at)}</span>
      <button className={PLAIN} disabled={answering === f.path} onClick={() => void decide(f.path, null)} title="Forget the answer: the file asks again" type="button">
        Forget
      </button>
    </div>
  );

  return (
    <Card
      action={
        <>
          {asks.length > 0 && <Chip tone="warn">{asks.length} ASKING</Chip>}
          {allowed.length > 0 && <Chip tone="ok">{allowed.length} ALLOWED</Chip>}
          {denied.length > 0 && <Chip>{denied.length} DENIED</Chip>}
          <span className="font-normal normal-case tracking-normal">
            <Freshness at={at} busy={busy} error={error} onReload={() => void reload()} />
          </span>
        </>
      }
      hue="var(--warn)"
      icon={<KeyRoundIcon className="size-3.5" />}
      title="Secret files"
      wide
    >
      <div className="flex flex-col gap-2 p-1" data-testid="secret-files">
        {asks.length > 0 && (
          <div className="flex min-w-0 items-start gap-2.5">
            <span className={`${LABEL} text-(--warn)`}>ASKING</span>
            <div className="flex min-w-0 flex-1 flex-col gap-1">
              {asks.map((a) => {
                const privateKey = a.kind === "private_key";
                return (
                  <div className="flex min-h-7 min-w-0 flex-wrap items-center gap-2" key={a.path}>
                    <span className="min-w-0 truncate font-mono text-(--fg)" title={a.path}>
                      {a.path}
                    </span>
                    <Chip tone={privateKey ? "bad" : "neutral"}>{KIND_WORD[a.kind] ?? KIND_WORD.unknown}</Chip>
                    <span className="min-w-0 truncate text-(--fg-dim) text-[10.5px]">
                      {bytes(a.bytes)} · {threadOf(a.stream_id)}
                      {a.program ? ` · ${a.program}` : ""} · {ago(a.at)}
                    </span>
                    <span className="flex-1" />
                    <button className={PLAIN} onClick={() => look(a)} type="button">
                      Look
                    </button>
                    <button className={PLAIN} disabled={answering === a.path} onClick={() => void decide(a.path, false)} type="button">
                      Deny
                    </button>
                    <button className={SOLID} disabled={answering === a.path} onClick={() => void decide(a.path, true, privateKey)} type="button">
                      Allow for this project
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        )}
        {allowed.length > 0 && (
          <div className="flex min-w-0 items-start gap-2.5">
            <span className={`${LABEL} text-(--ok)`}>ALLOWED</span>
            <div className="flex min-w-0 flex-1 flex-col">{allowed.map(answer)}</div>
          </div>
        )}
        {denied.length > 0 && (
          <div className="flex min-w-0 items-start gap-2.5">
            <span className={`${LABEL} text-(--fg-dim)`}>DENIED</span>
            <div className="flex min-w-0 flex-1 flex-col">{denied.map(answer)}</div>
          </div>
        )}
        {answerError && <p className="text-(--bad) text-[11px]">{answerError}</p>}
      </div>
    </Card>
  );
}
