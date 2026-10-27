// Every conversation grouped by where it stands — needs you, failed,
// working, done, idle — drawn with the vendored AI Elements `Queue` and the
// shadcn dropdown the agent panel already uses. The statuses are the node's
// (`SocketInfo.status`); the actions are the same list every other menu
// draws (`conversationActions`), run through the same runner.

import { Fragment, useState, useSyncExternalStore } from "react";
import { MoreHorizontalIcon } from "lucide-react";
import {
  Queue,
  QueueItem,
  QueueItemActions,
  QueueItemContent,
  QueueItemDescription,
  QueueItemIndicator,
  QueueSection,
  QueueSectionContent,
  QueueSectionLabel,
  QueueSectionTrigger,
} from "@/lib/ai-elements/queue";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";
import type { ConversationStatus } from "$lib/node";
import { ago, codePath, openPath, STATUSES, type ConversationAction } from "$lib/conversations";
import type { WorkBoardStore, WorkItem } from "./workBoardSource.svelte";

/** Status colours come from the Appearance tokens, never from here. */
const TONE: Record<ConversationStatus, string> = {
  needs_input: "border-transparent bg-[var(--warn)]",
  failed: "border-transparent bg-[var(--bad)]",
  working: "border-transparent bg-[var(--ok)] animate-pulse",
  done: "border-transparent bg-[var(--accent)]",
  idle: "",
};

export function WorkBoard({ store, emptyText }: { store: WorkBoardStore; emptyText: string }) {
  const snap = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const groups = STATUSES.map((s) => ({ ...s, items: snap.items.filter((i) => i.conv.status === s.status) })).filter(
    (g) => g.items.length > 0,
  );

  return (
    <div className="bakery-ai bakery-ai-board text-foreground" data-testid="work-board">
      {!snap.loaded ? (
        <p className="px-1 text-muted-foreground text-sm">Loading…</p>
      ) : groups.length === 0 ? (
        <p className="px-1 text-muted-foreground text-sm">{emptyText}</p>
      ) : (
        <Queue>
          {groups.map((g) => (
            <QueueSection key={g.status} defaultOpen data-testid={`work-${g.status}`}>
              <QueueSectionTrigger>
                <QueueSectionLabel
                  count={g.items.length}
                  label={g.label}
                  icon={<span className={cn("inline-block size-2 rounded-full border", TONE[g.status])} />}
                />
              </QueueSectionTrigger>
              <QueueSectionContent>
                <ul className="mt-1 mb-1 flex flex-col">
                  {g.items.map((item) => (
                    <BoardItem key={item.conv.streamId} item={item} store={store} />
                  ))}
                </ul>
              </QueueSectionContent>
            </QueueSection>
          ))}
        </Queue>
      )}
    </div>
  );
}

function BoardItem({ item, store }: { item: WorkItem; store: WorkBoardStore }) {
  const { conv, actions } = item;
  const [note, setNote] = useState<{ text: string; bad: boolean } | null>(null);

  async function run(a: ConversationAction) {
    setNote(null);
    try {
      const said = await store.run(a.id, conv.streamId);
      if (said) setNote({ text: said, bad: false });
    } catch (e) {
      setNote({ text: `${e}`.replace(/^Error:\s*/, ""), bad: true });
    }
  }

  const detail = [conv.place && !conv.main ? conv.place : "", conv.note, conv.busy ? "" : ago(conv.lastTs)]
    .filter(Boolean)
    .join(" · ");

  return (
    <QueueItem data-testid="work-item" data-status={conv.status}>
      <div className="flex items-center gap-2">
        <QueueItemIndicator className={TONE[conv.status]} />
        <QueueItemContent className="text-foreground">
          <a href={openPath(conv)} className="hover:underline" title={conv.baker ? "Open in the Baker" : "Open in Chat"}>
            {conv.title}
          </a>
        </QueueItemContent>
        {conv.pendingGates > 0 && (
          <a
            href={`${codePath(conv)}?open=review`}
            className="shrink-0 rounded-md border px-1.5 py-0.5 text-muted-foreground text-xs hover:text-foreground"
          >
            Review
          </a>
        )}
        <QueueItemActions>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon-xs"
                aria-label={`Thread actions: ${conv.title}`}
                data-testid="work-item-actions"
              >
                <MoreHorizontalIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {actions.map((a, i) => (
                <Fragment key={a.id}>
                  {i > 0 && actions[i - 1].group !== a.group && <DropdownMenuSeparator />}
                  <DropdownMenuItem
                    disabled={a.disabled}
                    variant={a.danger ? "destructive" : "default"}
                    title={a.hint}
                    onSelect={() => void run(a)}
                  >
                    {a.label}
                  </DropdownMenuItem>
                </Fragment>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </QueueItemActions>
      </div>
      {detail && <QueueItemDescription className="line-clamp-1">{detail}</QueueItemDescription>}
      {note && (
        <QueueItemDescription className={note.bad ? "text-destructive" : undefined} role="status">
          {note.text}
        </QueueItemDescription>
      )}
    </QueueItem>
  );
}
