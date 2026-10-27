// What each conversation action does — the one runner behind every actions
// menu (the Svelte menu and the AI Elements work board). Which actions apply
// is `conversationActions` in `conversations.ts`, pure so it is testable and
// importable from the React island.

import { goto } from "$app/navigation";
import { chatMakeProject, conversationRename } from "./api.generated";
import { ask } from "./ask";
import { askText } from "./askText";
import { pickFolder } from "./desktop";
import { node } from "./node";
import { studioStore } from "./studioStore.svelte";
import { chatPath, codePath, type Conversation, type ConversationActionId } from "./conversations";

/** Runs one action. Resolves to a sentence worth showing in the menu, or
 *  nothing when the menu should just close. Throws what the node refused. */
export async function runConversationAction(
  id: ConversationActionId,
  c: Conversation,
  here: string,
): Promise<string | void> {
  switch (id) {
    case "chat":
      return void goto(chatPath(c));
    case "code":
    case "baker":
      return void goto(codePath(c));
    case "rename": {
      const title = (await askText("Rename thread", c.title))?.trim();
      if (!title || title === c.title) return;
      await conversationRename({ stream_id: c.streamId, title });
      break;
    }
    case "stop": {
      const r = await node.cancelTurn(c.streamId, c.roleId);
      studioStore.refresh();
      return r.cancelled ? "Stopping its turn." : "Nothing was running.";
    }
    case "restart": {
      const r = await node.restartRuntime(c.streamId);
      studioStore.refresh();
      return r.restarted
        ? "Runtime stopped — its next message starts a fresh one with the whole history."
        : "No runtime was running — its next message starts one.";
    }
    case "archive":
    case "restore":
      await node.archiveConversation(c.streamId, id === "archive");
      break;
    case "project":
    case "project-folder": {
      let parent: string | undefined;
      if (id === "project-folder") {
        parent = (await pickFolder(`Where "${c.title}" goes`)) ?? undefined;
        if (!parent) return;
      }
      // The chat's title to start with; left empty, the node uses it too.
      const named = await askText("Project name", c.title);
      if (named === null) return;
      const name = named.trim() || c.title;
      const where = parent ?? "Bakery's projects folder";
      const ok = await ask(
        `Make "${name}" a project?\n\nIts files move to ${where} as a Git repository with a first commit, and the thread carries on there. Its runtime restarts.`,
      );
      if (!ok) return;
      const made = await chatMakeProject({ stream_id: c.streamId, parent, name });
      studioStore.refresh();
      return `"${made.name}" is a project now, in ${made.folder}.`;
    }
    case "delete": {
      const removed = c.bakerThread ? "" : c.chat ? "; its files move to the Trash" : "; its worktree and room are removed";
      const ok = await ask(
        `Delete "${c.title}" permanently?\n\nIt leaves Bakery and its runtime stops. Its history stays as an audit record${removed}. Archive instead if you may want it back.`,
      );
      if (!ok) return;
      const living = studioStore.findSocket(c.streamId)?.lifetime === "living";
      const wasHere = here === chatPath(c) || here === codePath(c);
      await studioStore.discard(c.streamId, living);
      if (wasHere) void goto(here.startsWith("/chat") ? "/chat" : c.bakerThread ? "/baker" : "/");
      return;
    }
  }
  studioStore.refresh();
}
