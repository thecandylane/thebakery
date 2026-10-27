// Code and Preview panels: one VS Code (openvscode-server) and one browser per
// worktree, each a native Chromium view the Electron shell lays over the
// panel (desktop.ts BakeryViews), with its own storage per kind and
// conversation. Two worktrees of the same app sit side by side, each with
// its own preview.
//
// The node starts them: POST /code/open answers the editor's URL, which
// carries its connection token, so that URL lives only in memory and in
// the view, and is asked for again on every mount (cheap while the editor
// runs). POST /preview/open answers the dev server's URL for a port; only
// the port is remembered. A view stays alive while its panel is hidden
// (another workspace, the floor) — the operator keeps many apps hot at once
// — and comes back as it was; it closes once its panel leaves every warm
// workspace (the shell calls closeViewsExcept).

import { useCallback, useEffect, useRef, useState } from "react";
import { BugIcon, EraserIcon, ExternalLinkIcon, RotateCwIcon } from "lucide-react";
import { codeOpen, previewOpen } from "$lib/api.generated";
import { ask } from "$lib/ask";
import { electronShell, electronViews, type ViewKind } from "$lib/desktop";
import { WorktreeStrip } from "./CodeView";
import { folderStream } from "./floorModel";
import { said, type ToolContext } from "./ui";
import { fullyInside } from "./viewModel";

/** What covers a native view (it paints above the page): menus, the
 *  palette, dialogs. While one is open, every view hides. */
const OVERLAY = '[role="dialog"], [role="alertdialog"], [role="menu"], [data-radix-popper-content-wrapper]';

/** Some node added or removed is, or holds, an overlay. */
function holdsOverlay(nodes: NodeList): boolean {
  for (const n of nodes) {
    if (n instanceof Element && (n.matches(OVERLAY) || n.querySelector(OVERLAY) !== null)) return true;
  }
  return false;
}

const iconButton = "rounded p-1 text-(--fg-faint) hover:bg-(--bg-chip) hover:text-(--fg-2) disabled:opacity-40";
const pill = "rounded-full border border-(--line) px-2 py-0.5 text-[11px] hover:bg-(--bg-chip) disabled:opacity-40";

/** Native views alive in Electron, by id, with the URL each holds — in
 *  memory only, as Code's URL carries the editor's token. */
const live = new Map<string, string>();

/** Closes every live view whose panel no warm workspace holds any more. */
export function closeViewsExcept(keep: ReadonlySet<string>) {
  const views = electronViews();
  for (const id of [...live.keys()]) {
    if (keep.has(id)) continue;
    live.delete(id);
    void views?.close(id);
  }
}

function readPort(key: string): string {
  try {
    return localStorage.getItem(key) ?? "";
  } catch {
    return "";
  }
}

function keepPort(key: string, port: string) {
  try {
    localStorage.setItem(key, port);
  } catch {
    // Not kept: the operator types it again next time.
  }
}

export function WebPanel({ ctx, kind, streamId }: { ctx: ToolContext; kind: ViewKind; streamId: string }) {
  const views = electronViews();
  const viewId = `${kind}:${streamId}`;
  const conv = ctx.snap.conversations.find((c) => c.streamId === streamId) ?? null;
  /** Whose folder it opens: a Baker conversation with none of its own opens the Bakery's. */
  const target = folderStream(ctx.snap.conversations, [...ctx.snap.projects, ctx.snap.bakery].flatMap((p) => p.checkouts), streamId);
  /** Memory only: Code's URL carries the editor's token. A view still alive
   *  shows at once with the URL it holds; the node is asked again anyway. */
  const [url, setUrl] = useState(() => live.get(viewId) ?? "");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [current, setCurrent] = useState("");
  const [title, setTitle] = useState("");
  const portKey = `bakery.preview.port.${target}`;
  const [port, setPort] = useState(() => readPort(portKey));

  useEffect(() => {
    if (!views) return;
    const offs = [
      views.onNavigate((id, u) => {
        if (id === viewId) setCurrent(u);
      }),
      views.onTitle((id, t) => {
        if (id === viewId) setTitle(t);
      }),
      views.onLoadFailed((id, e) => {
        if (id === viewId) setError(e);
      }),
    ];
    return () => offs.forEach((off) => off());
  }, [views, viewId]);

  const open = useCallback(async () => {
    setError("");
    setLoading(true);
    try {
      if (kind === "code") {
        setUrl((await codeOpen({ stream_id: target })).url);
      } else {
        const p = Number(port);
        if (!Number.isInteger(p) || p < 1 || p > 65535) throw new Error("Type the port this worktree's dev server listens on, e.g. 5173.");
        keepPort(portKey, String(p));
        setUrl((await previewOpen({ stream_id: target, port: p })).url);
      }
    } catch (e) {
      setError(said(e));
    } finally {
      setLoading(false);
    }
  }, [kind, port, portKey, target]);

  // On mount only: Code opens itself; Preview does when it remembers a port.
  useEffect(() => {
    if (views && (kind === "code" || port)) void open();
  }, [views, kind, target]);

  if (!views) {
    return (
      <p className="p-3 text-(--fg-faint) text-xs">{kind === "code" ? "Code" : "Preview"} · no native views in this window</p>
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col text-xs" data-testid={`${kind}-panel`}>
      <WorktreeStrip ctx={ctx} kind={kind === "code" ? "editor" : "preview"} streamId={target} />
      <div className="flex items-center gap-1 border-(--line-soft) border-b px-2 py-1">
        {kind === "code" ? (
          <span className="min-w-0 flex-1 truncate text-(--fg-dim)" title={title || undefined}>
            Code · {conv?.title ?? ""}
          </span>
        ) : (
          <form
            className="flex min-w-0 flex-1 items-center gap-1"
            onSubmit={(e) => {
              e.preventDefault();
              void open();
            }}
          >
            <input
              aria-label="Dev server port"
              className="w-20 rounded-md border border-(--line-soft) bg-(--bg-app) px-1.5 py-0.5 font-mono text-[11px] outline-none focus:border-(--accent-border)"
              inputMode="numeric"
              onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
              placeholder="port"
              value={port}
            />
            <button className={pill} disabled={loading || !port} type="submit">
              Open
            </button>
            <span className="min-w-0 flex-1 truncate font-mono text-(--fg-faint) text-[11px]" title={title || undefined}>
              {current || url}
            </span>
          </form>
        )}
        <button aria-label="Reload" className={iconButton} disabled={!url} onClick={() => views.reload(viewId)} title="Reload" type="button">
          <RotateCwIcon className="size-3.5" />
        </button>
        {kind === "preview" && (
          <>
            <button
              aria-label="Developer tools"
              className={iconButton}
              disabled={!url}
              onClick={() => views.openDevTools(viewId)}
              title="Developer tools"
              type="button"
            >
              <BugIcon className="size-3.5" />
            </button>
            <button
              aria-label="Clear this preview's data"
              className={iconButton}
              disabled={!url}
              onClick={async () => {
                if (await ask(`Clear this preview's cookies, storage and cache for ${conv?.title ?? "this worktree"}? It reloads signed out.`)) {
                  await views.clearData(viewId);
                }
              }}
              title="Clear data"
              type="button"
            >
              <EraserIcon className="size-3.5" />
            </button>
            {/* A preview's URL carries no secret; the editor's does, so Code has no such button. */}
            <button
              aria-label="Open in the browser"
              className={iconButton}
              disabled={!url}
              onClick={() => void electronShell()?.openExternal(current || url)}
              title="Open in the system browser"
              type="button"
            >
              <ExternalLinkIcon className="size-3.5" />
            </button>
          </>
        )}
      </div>
      {error && (
        <div className="flex items-start gap-2 border-(--warn-border) border-b bg-(--warn-bg) px-2 py-1 text-(--warn)" role="alert">
          <span className="min-w-0 flex-1 whitespace-pre-wrap break-words">{error}</span>
          {kind === "code" && (
            <button className={pill} disabled={loading} onClick={() => void open()} type="button">
              Try again
            </button>
          )}
        </div>
      )}
      {url ? (
        <NativeView id={viewId} key={url} kind={kind} streamId={streamId} url={url} />
      ) : (
        <p className="p-3 text-(--fg-faint)">
          {loading ? (kind === "code" ? "Starting Code…" : "Opening…") : ""}
        </p>
      )}
    </div>
  );
}

/** Keeps one native view glued to this placeholder: same place and size,
 *  shown only while it sits wholly inside its group or strip and nothing covers it. */
function NativeView({ id, kind, url, streamId }: { id: string; kind: ViewKind; url: string; streamId: string }) {
  const box = useRef<HTMLDivElement>(null);
  const [shown, setShown] = useState(false);

  useEffect(() => {
    const views = electronViews();
    const el = box.current;
    if (!views || !el) return;
    const strip = el.closest<HTMLElement>("[data-shell-clip]");
    let frame = 0;
    let visible = false;
    let placed = "";
    let alive = true;
    /** An overlay is open: asked again only as one may come or go. */
    let covered = document.querySelector(OVERLAY) !== null;
    const sync = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => {
        if (!alive) return;
        const r = el.getBoundingClientRect();
        const clip = (strip ?? document.documentElement).getBoundingClientRect();
        const now = fullyInside(r, clip) && !covered;
        const bounds = { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) };
        const key = JSON.stringify(bounds);
        if (now && key !== placed) {
          placed = key;
          void views.setBounds(id, bounds);
        }
        if (now !== visible) {
          visible = now;
          void views.setVisible(id, now);
          setShown(now);
        }
      });
    };
    // A view still alive with this URL is only shown again — nothing reloads.
    // One holding another URL (a restarted editor's new token) is replaced.
    const held = live.get(id);
    live.set(id, url);
    const ready =
      held === url
        ? Promise.resolve()
        : (held === undefined ? Promise.resolve() : views.close(id)).then(() => views.open({ id, kind, url, streamId }));
    void ready.then(sync);
    const resize = new ResizeObserver(sync);
    resize.observe(el);
    if (strip) resize.observe(strip);
    // Overlays come and go anywhere in the document, and the page around
    // the panel can move it, so any change places it again (one check per
    // frame); only one that adds or removes an overlay looks for overlays,
    // not every token a thread streams in.
    const overlays = new MutationObserver((records) => {
      if (records.some((r) => holdsOverlay(r.addedNodes) || holdsOverlay(r.removedNodes))) {
        covered = document.querySelector(OVERLAY) !== null;
      }
      sync();
    });
    overlays.observe(document.body, { childList: true, subtree: true });
    window.addEventListener("resize", sync);
    document.addEventListener("scroll", sync, true);
    return () => {
      alive = false;
      cancelAnimationFrame(frame);
      resize.disconnect();
      overlays.disconnect();
      window.removeEventListener("resize", sync);
      document.removeEventListener("scroll", sync, true);
      // Hidden, not closed: it waits, as it was, for its panel to show again.
      void views.setVisible(id, false);
    };
  }, [id, kind, url, streamId]);

  return <div className="relative min-h-0 flex-1 bg-(--bg-app)" data-shown={shown || undefined} ref={box} />;
}
