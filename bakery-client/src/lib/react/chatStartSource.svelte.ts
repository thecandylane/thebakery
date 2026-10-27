// The Chat home composer's side of the Svelte/React boundary — the same
// subscription adapter as `workBoardSource.svelte.ts`: one `$effect.root`
// rebuilds a frozen snapshot from the page's state and notifies React. The
// page keeps every decision (which chat, which account, create-then-send);
// the island only draws them with AI Elements and reports choices back.

export type ChatStartChoice = Readonly<{ id: string; label: string }>;

export type ChatStartSnapshot = Readonly<{
  /** `NEW_CHAT`, or the stream id of the chat the message continues. */
  target: string;
  /** Active chats a message can be sent to instead. */
  chats: readonly ChatStartChoice[];
  /** Accounts that can run a new chat right now. */
  accounts: readonly ChatStartChoice[];
  account: string;
  /** The account's own models; "" is the account default. */
  models: readonly ChatStartChoice[];
  model: string;
  /** The chosen model's effort levels; "" is the default. */
  efforts: readonly ChatStartChoice[];
  effort: string;
  /** For an existing chat: the model it runs on. */
  chatModel: string;
  /** Where the account's model list stands: being read (`loading`), read
   *  (`ready`), or not readable (`unavailable`, with `modelsReason`). `none`
   *  when the account has no model list to offer. */
  modelsState: "loading" | "ready" | "unavailable" | "none";
  modelsReason: string;
  sending: boolean;
  error: string;
}>;

/** A file from the composer, already read to a data URL. */
export type ChatStartFile = Readonly<{ url?: string; filename?: string; mediaType?: string }>;

export type ChatStartActions = {
  setTarget(id: string): void;
  setAccount(id: string): void;
  setModel(id: string): void;
  setEffort(id: string): void;
  /** Asks the account's adapter for its models again. */
  retryModels(): void;
  /** Resolves once sent. Rejects on refusal, which keeps the draft. */
  send(text: string, files: readonly ChatStartFile[]): Promise<void>;
};

export type ChatStartStore = {
  subscribe(onChange: () => void): () => void;
  getSnapshot(): ChatStartSnapshot;
  actions: ChatStartActions;
  dispose(): void;
};

export const NEW_CHAT = "new";

export function createChatStartSource(read: () => ChatStartSnapshot, actions: ChatStartActions): ChatStartStore {
  const listeners = new Set<() => void>();
  const build = () => Object.freeze({ ...read() });
  let snapshot = build();
  const stopRoot = $effect.root(() => {
    $effect(() => {
      snapshot = build();
      for (const notify of listeners) notify();
    });
  });
  return {
    subscribe(onChange) {
      listeners.add(onChange);
      return () => listeners.delete(onChange);
    },
    getSnapshot: () => snapshot,
    actions,
    dispose() {
      stopRoot();
      listeners.clear();
    },
  };
}
