// The Mac's notification for a host a thread's sandbox asked to reach
// (Round 7, H3): Allow for this project or Deny without coming back to the
// app. Shown only while the app isn't the window in front, since the card in
// the thread is the main way to answer; taken down the moment the ask is
// answered anywhere (the card, the list, the phone, another notification).
// A reload took the old ones down, so asks still open notify again.

import { useEffect, useRef } from "react";
import { filesReadAsks, networkHostDecide, networkHosts } from "$lib/api.generated";
import { electronShell } from "$lib/desktop";
import { onEnvelope, onEvent } from "$lib/events";
import { covers, sharedTenant } from "../hostsModel";
import { SHARED_WARNING } from "./Internet";
import type { ShellSnapshot } from "./shellSource.svelte";

const ALLOW = "Allow for this project";
const DENY = "Deny";
type HostAsk = { projectId: string; host: string; streamId: string; program: string | null };

function payloadOf(raw: string): Record<string, unknown> {
  try {
    const v = JSON.parse(raw);
    return v && typeof v === "object" ? v : {};
  } catch {
    return {};
  }
}

export function useHostAskNotices(snap: ShellSnapshot, openThread: (streamId: string) => void) {
  const latest = useRef(snap);
  latest.current = snap;
  const open = useRef(openThread);
  open.current = openThread;
  const notify = useRef<((ask: HostAsk) => void) | null>(null);

  useEffect(() => {
    const shell = electronShell();
    if (!shell) return;
    /** The notices up now, one per project and host, as the node asks. */
    const shown = new Map<string, HostAsk>();
    notify.current = (ask) => {
      const id = `host-ask:${ask.projectId}:${ask.host}`;
      if (shown.has(id) || document.hasFocus()) return;
      shown.set(id, ask);
      const s = latest.current;
      const thread = s.conversations.find((c) => c.streamId === ask.streamId)?.title ?? "A thread";
      const project = s.projects.find((p) => p.id === ask.projectId)?.name;
      const body = [ask.program ? `from ${ask.program}` : null, project ? `for ${project}` : null, sharedTenant(ask.host) ? SHARED_WARNING : null]
        .filter(Boolean)
        .join(" · ");
      const gone = () => {
        if (shown.get(id) === ask) shown.delete(id);
      };
      shell.notify({ id, title: `${thread} wants to reach ${ask.host}`, body: body || ask.host, actions: [ALLOW, DENY] }).then((pressed) => {
        gone();
        if (pressed === ALLOW || pressed === DENY) {
          void networkHostDecide({ project_id: ask.projectId, host: ask.host, allow: pressed === ALLOW }).catch(() => {});
        } else if (pressed === "open") open.current(ask.streamId);
      }, gone);
    };
    const off = onEvent(null, (ev) => {
      if (ev.family !== "network.host.ask" && ev.family !== "network.host.decided") return;
      const p = payloadOf(ev.payload);
      const projectId = typeof p.project_id === "string" ? p.project_id : "";
      const host = typeof p.host === "string" ? p.host : "";
      if (!projectId || !host) return;
      if (ev.family === "network.host.ask") {
        // The thread's own copy, or the project's, which names the thread.
        const streamId = typeof p.stream_id === "string" && p.stream_id ? p.stream_id : ev.stream_id;
        notify.current?.({ projectId, host, streamId, program: typeof p.program === "string" && p.program ? p.program : null });
        return;
      }
      // Answered anywhere: whatever it answers comes down.
      for (const [id, a] of shown) {
        if (a.projectId === projectId && covers(host, a.host)) {
          shown.delete(id);
          void shell.closeNotification(id);
        }
      }
    });
    return () => {
      notify.current = null;
      off();
    };
  }, []);

  // Asks still open once the shell knows its projects: a reload took their
  // notices down.
  useEffect(() => {
    if (!snap.loaded || !notify.current) return;
    for (const p of latest.current.projects) {
      networkHosts({ project_id: p.id }).then(
        (h) => {
          for (const a of h.asks) notify.current?.({ projectId: p.id, host: a.host, streamId: a.stream_id, program: a.program ?? null });
        },
        () => {},
      );
    }
  }, [snap.loaded]);
}

type FileAsk = { projectId: string; path: string; streamId: string };

/** The Mac's notification for a secret-named file a project's build was
 *  refused (the operator's answer): it only opens the thread's card, where
 *  the answer is (a private key takes a second yes there). Shown while the
 *  app isn't in front; taken down once the file is answered anywhere. */
export function useFileAskNotices(snap: ShellSnapshot, openThread: (streamId: string) => void) {
  const latest = useRef(snap);
  latest.current = snap;
  const open = useRef(openThread);
  open.current = openThread;
  const notify = useRef<((ask: FileAsk) => void) | null>(null);

  useEffect(() => {
    const shell = electronShell();
    if (!shell) return;
    const shown = new Map<string, FileAsk>();
    notify.current = (ask) => {
      const id = `file-ask:${ask.projectId}:${ask.path}`;
      if (shown.has(id) || document.hasFocus()) return;
      shown.set(id, ask);
      const project = latest.current.projects.find((p) => p.id === ask.projectId)?.name ?? "A project";
      const gone = () => {
        if (shown.get(id) === ask) shown.delete(id);
      };
      shell.notify({ id, title: `${project}'s build wants to read ${ask.path}`, body: "Click to review", actions: [] }).then((pressed) => {
        gone();
        if (pressed === "open") open.current(ask.streamId);
      }, gone);
    };
    const off = onEvent(null, (ev) => {
      if (ev.family !== "file.read.ask" && ev.family !== "file.read.decided") return;
      const p = payloadOf(ev.payload);
      const projectId = typeof p.project_id === "string" ? p.project_id : "";
      const path = typeof p.path === "string" ? p.path : "";
      if (!projectId || !path) return;
      if (ev.family === "file.read.ask") {
        // The thread's own copy, or the project's, which names the thread.
        notify.current?.({ projectId, path, streamId: typeof p.stream_id === "string" && p.stream_id ? p.stream_id : ev.stream_id });
        return;
      }
      const id = `file-ask:${projectId}:${path}`;
      if (shown.delete(id)) void shell.closeNotification(id);
    });
    return () => {
      notify.current = null;
      off();
    };
  }, []);

  // Asks still open once the shell knows its projects.
  useEffect(() => {
    if (!snap.loaded || !notify.current) return;
    for (const p of latest.current.projects) {
      filesReadAsks({ project_id: p.id }).then(
        (f) => {
          for (const a of f.asks) notify.current?.({ projectId: p.id, path: a.path, streamId: a.stream_id });
        },
        () => {},
      );
    }
  }, [snap.loaded]);
}

/** The learning loop's nightly digest on the Mac (the node sends it only
 *  while the digest's Mac notification is on): it opens the memory wiki on
 *  its To keep. */
export function useDigestNotices(snap: ShellSnapshot) {
  const latest = useRef(snap);
  latest.current = snap;
  useEffect(() => {
    const shell = electronShell();
    if (!shell) return;
    return onEnvelope("memory.digest", (e) => {
      const n = (k: string) => (typeof e[k] === "number" ? (e[k] as number) : 0);
      const projectId = typeof e.project === "string" ? e.project : null;
      const project = projectId ? latest.current.projects.find((p) => p.id === projectId)?.name : null;
      const pending = n("pending");
      const stale = n("stale");
      shell
        .notify({
          id: `memory-digest:${projectId ?? "bakery"}`,
          title: `${project ? `${project}'s memory` : "Memory"}: ${pending ? `${pending} to keep` : "nothing to keep"}`,
          body: stale ? `${stale} ${stale === 1 ? "note" : "notes"} went stale` : "Click to review",
          actions: [],
        })
        .then((pressed) => {
          if (pressed === "open") window.dispatchEvent(new CustomEvent("bakery:open-tool", { detail: { id: "@memory", tab: "pages" } }));
        }, () => {});
    });
  }, []);
}
