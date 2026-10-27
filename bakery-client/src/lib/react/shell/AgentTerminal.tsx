// A thread's Agent terminal as a tab of its own, beside the thread: every
// command its agents ran, as `$ command`, its output and `[exit N]`,
// read-only. It opens only from a command's row in the thread, on that
// command's block (terminalSession.ts finds it by its call's mark); the
// terminal bar is the operator's own and never shows it.

import { useEffect, useRef, useState } from "react";
import { terminalCreate } from "$lib/api.generated";
import { type CommandAt, revealCall } from "$lib/terminalSession";
import { mountTerminal } from "./svelteTerminal";
import { said } from "./ui";

/** The command each thread's Agent tab is to show, until the tab has its terminal. */
const wanted = new Map<string, CommandAt>();
const wanting = new Set<() => void>();

/** Brings a command's block forward in its thread's Agent tab (the shell opens the tab). */
export function showCommand(streamId: string, at: CommandAt) {
  wanted.set(streamId, at);
  for (const take of wanting) take();
}

export function AgentTerminal({ streamId }: { streamId: string }) {
  const box = useRef<HTMLDivElement>(null);
  const [terminalId, setTerminalId] = useState<string | null>(null);
  const [error, setError] = useState("");

  // The node opens the thread's Agent terminal, making it if it has none yet.
  useEffect(() => {
    let live = true;
    setTerminalId(null);
    setError("");
    terminalCreate({ stream_id: streamId, mode: "agent", name: null, command: null }).then(
      (t) => live && setTerminalId(t.terminal_id),
      (e) => live && setError(said(e)),
    );
    return () => {
      live = false;
    };
  }, [streamId]);

  useEffect(() => {
    if (!box.current || !terminalId) return;
    const stop = mountTerminal(box.current, {
      streamId,
      terminalId,
      name: "Agent",
      readOnly: true,
      bare: true,
      keepAlive: true,
      onClosed: () => {},
    });
    const take = () => {
      const at = wanted.get(streamId);
      if (!at) return;
      wanted.delete(streamId);
      revealCall(streamId, terminalId, at);
    };
    take();
    wanting.add(take);
    return () => {
      wanting.delete(take);
      stop();
    };
  }, [streamId, terminalId]);

  if (error) return <div className="grid h-full place-items-center p-4 text-(--bad) text-xs">{error}</div>;
  return <div className="h-full min-h-0 bg-(--bg-rail)" data-testid="agent-terminal" ref={box} />;
}
