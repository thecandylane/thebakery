// The Chat home's composer: the agent pane's AI Elements prompt, drawing the
// page's decisions — where the message goes (a new chat or an active one)
// and, for a new chat, its account, model and effort — and reporting each
// pick back. Sending is the page's too: it creates the chat when needed,
// queues the message and opens the chat. Nothing is decided here.
//
// Two behaviours are load-bearing (the pane's `Composer.tsx` has both):
//
//  1. Draft preservation. Inside a `PromptInputProvider`, `PromptInput`
//     clears the box only when `onSubmit` RESOLVES. Without one it calls
//     `form.reset()` before `onSubmit` even runs, which loses the text on a
//     refusal — and resets every picker in the form to the value it mounted
//     with. So the page's `send` rejects unless the message was sent.
//  2. The Enter guard. `PromptInputTextarea` submits on Enter unless it finds
//     a disabled `button[type="submit"]` in the form, so the submit button is
//     always rendered, disabled whenever there is nothing to send or nowhere
//     to send it.

import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import { CheckIcon, ChevronDownIcon, PaperclipIcon } from "lucide-react";
import {
  PromptInput,
  PromptInputAttachment,
  PromptInputAttachments,
  PromptInputBody,
  PromptInputButton,
  PromptInputFooter,
  type PromptInputMessage,
  PromptInputProvider,
  PromptInputSelect,
  PromptInputSelectContent,
  PromptInputSelectItem,
  PromptInputSelectTrigger,
  PromptInputSubmit,
  PromptInputTextarea,
  PromptInputTools,
  usePromptInputAttachments,
  usePromptInputController,
} from "@/lib/ai-elements/prompt-input";
import {
  ModelSelector,
  ModelSelectorContent,
  ModelSelectorEmpty,
  ModelSelectorGroup,
  ModelSelectorInput,
  ModelSelectorItem,
  ModelSelectorList,
  ModelSelectorName,
  ModelSelectorTrigger,
} from "@/lib/ai-elements/model-selector";
import { NEW_CHAT, type ChatStartChoice, type ChatStartStore } from "./chatStartSource.svelte";
import { registerAttachTarget } from "./attachTargets";
import { MAX_ATTACHMENT_BYTES } from "$lib/attachmentInput";

/** Radix Select reserves the empty string, so "the model's default effort"
 *  needs a sentinel. It is mapped back to "" before it reaches the page. */
const DEFAULT_EFFORT = "__default__";

/** Rejecting with this leaves the draft in the box (see the note above). */
const NOT_SENT = new Error("nothing was sent");

/** shadcn's `SelectTrigger` sets its height with `data-[size=default]:h-9`,
 *  which outranks a plain `h-7`; only the same variant makes it compact. */
const TRIGGER = "data-[size=default]:h-7 min-w-0 max-w-56 px-2 py-0 text-xs";

/** Inside a form, Radix mirrors each Select into a hidden native `<select>`.
 *  When the page moves a value before its option exists — the chat created
 *  a moment ago, an effort only the newly picked model has — that element
 *  reports "" through `onValueChange`. Only a value the menu offers is a
 *  pick. */
function offered(choices: readonly ChatStartChoice[], id: string): boolean {
  return choices.some((c) => c.id === id);
}

export function ChatStart({ store }: { store: ChatStartStore }) {
  return (
    <PromptInputProvider>
      <ChatStartInner store={store} />
    </PromptInputProvider>
  );
}

function ChatStartInner({ store }: { store: ChatStartStore }) {
  const snap = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const text = usePromptInputController().textInput.value;
  const attachments = usePromptInputAttachments();
  const [modelOpen, setModelOpen] = useState(false);
  const [dropping, setDropping] = useState(false);
  // Why a file was not added (too big, too many) — never a silent refusal.
  const [draftNotice, setDraftNotice] = useState("");
  const { actions } = store;

  // Files dropped or pasted anywhere on the page attach here (attachTargets).
  const addRef = useRef(attachments.add);
  addRef.current = attachments.add;
  const attachTarget = useRef<ReturnType<typeof registerAttachTarget> | null>(null);
  useEffect(() => {
    const target = registerAttachTarget({
      add: (files) => {
        addRef.current(files);
        return true;
      },
      setDropping,
    });
    attachTarget.current = target;
    return () => target.dispose();
  }, []);

  const isNew = snap.target === NEW_CHAT;
  const noAccount = isNew && snap.accounts.length === 0;
  const showModels = isNew && !noAccount && snap.models.length > 0;
  const targetLabel = isNew ? "New thread" : (snap.chats.find((c) => c.id === snap.target)?.label ?? "…");
  const accountLabel = snap.accounts.find((a) => a.id === snap.account)?.label ?? "Account";
  const modelLabel = snap.models.find((m) => m.id === snap.model)?.label ?? "Default model";
  const effort = snap.efforts.find((l) => l.id === snap.effort) ?? null;

  // A catalog can vanish under the open model picker (its account stopped
  // being runnable); the picker must not reopen by itself when one returns.
  useEffect(() => {
    if (!showModels) setModelOpen(false);
  }, [showModels]);

  async function submit(message: PromptInputMessage) {
    setDraftNotice("");
    const instruction = message.text.trim();
    if ((instruction === "" && message.files.length === 0) || snap.sending) throw NOT_SENT;
    await actions.send(instruction, message.files);
  }

  function pickModel(id: string) {
    actions.setModel(id);
    setModelOpen(false);
  }

  return (
    <div
      className="bakery-ai relative text-foreground"
      data-testid="chat-start"
      onFocusCapture={() => attachTarget.current?.activate()}
      onPointerDownCapture={() => attachTarget.current?.activate()}
    >
      {dropping && (
        <div
          className="pointer-events-none absolute inset-0 z-20 flex items-center justify-center rounded-xl border-2 border-primary/60 border-dashed bg-background/70 text-sm"
          data-testid="drop-overlay"
        >
          Drop to attach
        </div>
      )}
      <PromptInput
        className="bakery-ai-prompt rounded-xl shadow-sm"
        maxFileSize={MAX_ATTACHMENT_BYTES}
        multiple
        onError={(issue) => setDraftNotice(issue.message)}
        onSubmit={submit}
      >
        <PromptInputBody>
          <PromptInputAttachments className="px-0 pb-2 pt-0">
            {(attachment) => <PromptInputAttachment data={attachment} />}
          </PromptInputAttachments>
          <PromptInputTextarea aria-label="Message" autoFocus placeholder="Message…" />
        </PromptInputBody>

        <PromptInputFooter className="flex-wrap">
          <PromptInputTools className="min-w-0 flex-1 flex-wrap">
            <PromptInputButton
              aria-label="Attach files"
              data-testid="chat-start-attach"
              disabled={snap.sending}
              onClick={() => attachments.openFileDialog()}
              title="Attach files — or drop or paste them anywhere"
            >
              <PaperclipIcon className="size-4" />
            </PromptInputButton>
            <PromptInputSelect
              disabled={snap.sending}
              onValueChange={(id) => {
                if (id === NEW_CHAT || offered(snap.chats, id)) actions.setTarget(id);
              }}
              value={snap.target}
            >
              <PromptInputSelectTrigger
                aria-label="Send to"
                className={TRIGGER}
                data-testid="chat-start-target"
                title={targetLabel}
              >
                <span className="truncate">{targetLabel}</span>
              </PromptInputSelectTrigger>
              <PromptInputSelectContent className="max-w-80" position="popper">
                <PromptInputSelectItem value={NEW_CHAT}>New thread</PromptInputSelectItem>
                {snap.chats.map((c) => (
                  <PromptInputSelectItem key={c.id} value={c.id}>
                    {c.label}
                  </PromptInputSelectItem>
                ))}
              </PromptInputSelectContent>
            </PromptInputSelect>

            {!isNew ? (
              snap.chatModel ? (
                <span className="min-w-0 truncate px-1 text-xs" data-testid="chat-start-chat-model" title={snap.chatModel}>
                  {snap.chatModel}
                </span>
              ) : null
            ) : noAccount ? (
              <a className="px-1 text-xs" href="/settings/providers">
                Add a model
              </a>
            ) : (
              <>
                <PromptInputSelect
                  disabled={snap.sending}
                  onValueChange={(id) => {
                    if (offered(snap.accounts, id)) actions.setAccount(id);
                  }}
                  value={snap.account}
                >
                  <PromptInputSelectTrigger
                    aria-label="Account"
                    className={TRIGGER}
                    data-testid="chat-start-account"
                    title={accountLabel}
                  >
                    <span className="truncate">{accountLabel}</span>
                  </PromptInputSelectTrigger>
                  <PromptInputSelectContent className="max-w-80" position="popper">
                    {snap.accounts.map((a) => (
                      <PromptInputSelectItem key={a.id} value={a.id}>
                        {a.label}
                      </PromptInputSelectItem>
                    ))}
                  </PromptInputSelectContent>
                </PromptInputSelect>

                {/* Never silently absent: the list is being read, or why it
                    could not be — and a Retry that asks the adapter again. */}
                {snap.modelsState === "loading" && (
                  <span className="px-1 text-muted-foreground text-xs" data-testid="chat-start-models-loading" role="status">
                    Loading models…
                  </span>
                )}
                {snap.modelsState === "unavailable" && (
                  <span
                    className="flex min-w-0 max-w-96 items-center gap-1.5 px-1 text-muted-foreground text-xs"
                    data-testid="chat-start-models-unavailable"
                    role="status"
                    title={snap.modelsReason}
                  >
                    <span className="truncate">Models unavailable{snap.modelsReason ? ` — ${snap.modelsReason}` : ""}</span>
                    <PromptInputButton
                      className="h-6 shrink-0 px-2 text-xs"
                      data-testid="chat-start-models-retry"
                      onClick={() => actions.retryModels()}
                      variant="ghost"
                    >
                      Retry
                    </PromptInputButton>
                  </span>
                )}

                {showModels && (
                  <ModelSelector onOpenChange={setModelOpen} open={modelOpen}>
                    <ModelSelectorTrigger asChild>
                      <PromptInputButton
                        className="h-7 min-w-0 max-w-56 text-xs"
                        data-testid="chat-start-model"
                        disabled={snap.sending}
                        title={modelLabel}
                        variant="ghost"
                      >
                        <span className="truncate">{modelLabel}</span>
                        <ChevronDownIcon className="size-3 shrink-0" />
                      </PromptInputButton>
                    </ModelSelectorTrigger>
                    <ModelSelectorContent className="max-w-md" title="Choose model">
                      <ModelSelectorInput placeholder="Search models…" />
                      <ModelSelectorList>
                        <ModelSelectorEmpty>No matching model.</ModelSelectorEmpty>
                        <ModelSelectorGroup>
                          <ModelSelectorItem onSelect={() => pickModel("")} value="Default model">
                            <CheckIcon className={`size-3.5 ${snap.model === "" ? "opacity-100" : "opacity-0"}`} />
                            <ModelSelectorName>Default model</ModelSelectorName>
                          </ModelSelectorItem>
                          {snap.models.map((m) => (
                            <ModelSelectorItem key={m.id} onSelect={() => pickModel(m.id)} value={`${m.label} ${m.id}`}>
                              <CheckIcon
                                className={`size-3.5 ${snap.model === m.id ? "opacity-100" : "opacity-0"}`}
                              />
                              <ModelSelectorName>{m.label}</ModelSelectorName>
                              {m.label !== m.id && (
                                <span className="truncate font-mono text-muted-foreground text-xs">{m.id}</span>
                              )}
                            </ModelSelectorItem>
                          ))}
                        </ModelSelectorGroup>
                      </ModelSelectorList>
                    </ModelSelectorContent>
                  </ModelSelector>
                )}

                {snap.efforts.length > 0 && (
                  <PromptInputSelect
                    disabled={snap.sending}
                    onValueChange={(id) => {
                      if (id === DEFAULT_EFFORT) actions.setEffort("");
                      else if (offered(snap.efforts, id)) actions.setEffort(id);
                    }}
                    value={effort?.id ?? DEFAULT_EFFORT}
                  >
                    <PromptInputSelectTrigger aria-label="Effort" className={TRIGGER} data-testid="chat-start-effort">
                      <span className="truncate">{effort?.label ?? "Default effort"}</span>
                    </PromptInputSelectTrigger>
                    <PromptInputSelectContent position="popper">
                      <PromptInputSelectItem value={DEFAULT_EFFORT}>Default effort</PromptInputSelectItem>
                      {snap.efforts.map((l) => (
                        <PromptInputSelectItem key={l.id} value={l.id}>
                          {l.label}
                        </PromptInputSelectItem>
                      ))}
                    </PromptInputSelectContent>
                  </PromptInputSelect>
                )}
              </>
            )}
          </PromptInputTools>

          <PromptInputTools className="self-end">
            {/* Always rendered — see the Enter-guard note at the top. */}
            <PromptInputSubmit
              aria-label="Send"
              data-testid="chat-start-send"
              disabled={snap.sending || (text.trim() === "" && attachments.files.length === 0) || noAccount}
              status={snap.sending ? "submitted" : undefined}
              title="Send"
            />
          </PromptInputTools>
        </PromptInputFooter>
      </PromptInput>

      {draftNotice && (
        <p className="mt-1.5 px-1 text-destructive text-xs" data-testid="chat-start-notice" role="status">
          {draftNotice}
        </p>
      )}
      {snap.error && (
        <p className="mt-1.5 px-1 text-destructive text-xs" data-testid="chat-start-error" role="alert">
          {snap.error}
        </p>
      )}
    </div>
  );
}
