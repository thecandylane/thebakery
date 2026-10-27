// The composer: one textarea, the controls that genuinely reach Bakery, and
// nothing else.
//
// Three behaviours here are load-bearing and easy to break:
//
//  1. Draft preservation. Upstream's `PromptInput` clears the box when
//     `onSubmit` RESOLVES and leaves it alone when it REJECTS. So a refusal
//     must throw. That is the whole mechanism — there is no second copy of
//     the text to restore from.
//  2. The Enter guard. `PromptInputTextarea` submits on Enter unless it
//     finds a disabled `button[type="submit"]` in the form. A submit button
//     is therefore always rendered, disabled only when the agent cannot
//     accept a durable instruction. Replacing it with a Stop button would
//     make Enter bypass that guard and lose the queue affordance.
//  3. Key interception. That same textarea spreads caller props AFTER its
//     own `onKeyDown`, so passing one would REPLACE its Enter/IME handling.
//     The slash menu therefore listens in the capture phase on a wrapper and
//     stops propagation only for the keys it consumes, leaving upstream
//     intact.

import {
  type MutableRefObject,
  type ReactNode,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  PromptInput,
  PromptInputAttachment,
  PromptInputAttachments,
  PromptInputBody,
  PromptInputButton,
  PromptInputFooter,
  PromptInputHeader,
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
  Context,
  ContextContent,
  ContextContentBody,
  ContextContentHeader,
  ContextTrigger,
} from "@/lib/ai-elements/context";
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
import { NativeModelPicker } from "./NativeModelPicker";
import { providersOffered, waitingProvider, waitingSwitchNote } from "./claudeProviders";
import { useCached } from "./shell/cache";
import { EffortPicks, type EffortHost } from "./effortPicks";
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from "@/components/ui/command";
import { Badge } from "@/components/ui/badge";
import {
  ArrowUpIcon,
  CheckIcon,
  ChevronDownIcon,
  FileIcon,
  FolderIcon,
  PaperclipIcon,
  PlayIcon,
  PlusIcon,
  SlashIcon,
  SquareIcon,
  TimerIcon,
  XIcon,
  ZapIcon,
} from "lucide-react";
import type {
  AccountCapabilities,
  AcpRuntimeSnapshot,
  DirEntry,
  EffortLevel,
  ModelCatalog,
  OperatingMode,
} from "$lib/node";
import { effortLevelsForOverride } from "$lib/studioTarget";
import { authStatus, search } from "$lib/api.generated";
import { MAX_ATTACHMENT_BYTES } from "$lib/attachmentInput";
import { promptTokens, type TranscriptItem } from "$lib/transcript";
import type { AgentPanelActions, AgentPanelSnapshot } from "./contract";
import {
  activityGauge,
  contextGauge,
  Draftbox,
  latestUsage,
  type HttpTurnOverride,
  type QueuedItem,
  shortRuntime,
  toEndOfDraft,
  withConfigValue,
} from "./panelState";
import { fromComposer } from "./composerKeys";
import { availableCommands, filterCommands, parseSlash, type SlashCommand } from "./panelCommands";
import {
  claimPendingTurn,
  hasPendingTurn,
  pendingTurnKey,
  releasePendingTurn,
  subscribePendingTurn,
} from "./composerPendingTurns";

/** Radix Select reserves the empty string, so "use the seat's own model"
 *  needs a sentinel. It is mapped back to "" — i.e. "send no override" —
 *  before the request is built. */
const SEAT_DEFAULT = "__seat_default__";

/** shadcn's `SelectTrigger` sets its height with `data-[size=default]:h-9`,
 *  which outranks a plain `h-7`; only the same variant makes the pickers as
 *  compact as the buttons beside them. */
const SELECT_TRIGGER = "data-[size=default]:h-7 px-2 py-0 text-xs";

/** The toolbar's square buttons, framed as in look C. */
const SQUARE =
  "size-6 shrink-0 rounded-[5px] border border-(--line) bg-(image:--grad-button) text-(--fg-2) hover:text-(--fg) disabled:opacity-40";
/** The one accent button: send, or stop while a turn runs and nothing is typed. */
const ACCENT =
  "size-7 shrink-0 rounded-md border border-(--accent-fg) bg-(image:--grad-accent) text-(--fg-on-solid) hover:brightness-105 disabled:opacity-40";
/** A labelled toolbar button (Steer, Resume) at the pills' size. */
const PILL = "h-[22px] gap-1 rounded-[5px] px-2 text-[11px] [&_svg]:size-3";
/** ⚡ the operating mode, as a pill. */
const MODE_TRIGGER =
  "data-[size=default]:h-[22px] shrink-0 gap-1 rounded-[5px] border border-solid border-(--accent-border) bg-(--bg-pressed) px-2 py-0 text-(--accent-fg) text-[11px] shadow-none";

/** A pill that opens a chooser: the seat's runtime, the model and effort. */
const PILL_TRIGGER =
  "h-[22px] min-w-0 max-w-40 gap-1.5 rounded-[5px] border border-(--line) bg-(--bg-chip) px-2 text-(--fg-body) text-[11px] shadow-none hover:bg-(--bg-chip) hover:text-(--fg) [&_svg]:size-3";
// `border-solid`: upstream's trigger sets `border-none`, a border style.
const SEAT_TRIGGER = `${PILL_TRIGGER} border-solid data-[size=default]:h-[22px] py-0`;

/** The seat's assignment — the runtime this agent runs on (Claude Code, Grok,
 *  …) — and the control that changes it (`POST /seat/assign`). That is a
 *  different thing from the model pill's per-turn choice beside it, and it
 *  never touches history. */
function SeatPill({
  snap,
  actions,
  open,
  onOpenChange,
}: {
  snap: AgentPanelSnapshot;
  actions: AgentPanelActions;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const [error, setError] = useState("");
  // A switch that couldn't land shows until the picker is next opened.
  const [failureSeen, setFailureSeen] = useState<string | null>(null);
  const inFlight = useRef(false);
  const assign = useCallback(
    async (modelId: string) => {
      // Back to the runtime it is on cancels a switch waiting for the turn
      // to end, on the provider it has; otherwise it asks nothing.
      const back = modelId === snap.assignedModelId;
      if (inFlight.current || (back && !snap.switching)) return;
      inFlight.current = true;
      setError("");
      const out = await actions.assignModel(modelId, back ? (snap.assignedProvider ?? undefined) : undefined);
      if (out.status === "error") setError(out.error);
      inFlight.current = false;
    },
    [actions, snap.assignedModelId, snap.assignedProvider, snap.switching],
  );
  const label = (id: string) => shortRuntime(snap.availableModels.find((m) => m.id === id)?.label ?? id);
  // A switch to another runtime waiting for this turn to end.
  const waiting =
    snap.switching && snap.switching.modelId !== snap.assignedModelId
      ? waitingSwitchNote(snap.switching, snap.assignedModelId, snap.busy, label)
      : null;
  const failed = snap.switchFailed && snap.switchFailed.id !== failureSeen ? snap.switchFailed : null;
  const failure = failed ? `Couldn't switch to ${waitingSwitchNote(failed, snap.assignedModelId, false, label)?.to}: ${failed.error}` : "";
  // Nothing assignable and nothing assigned: say so rather than offer an
  // empty picker. An assigned runtime still shows when none is ready.
  if (snap.availableModels.length === 0 && snap.assignedModelId == null) {
    return (
      <span className="px-1 text-(--fg-dim) text-[11px]" data-testid="agent-model" title="No models are ready for this thread yet.">
        unassigned
      </span>
    );
  }
  const assigned = snap.availableModels.find((m) => m.id === snap.assignedModelId);
  return (
    <>
      {/* While a switch waits, the list holds its runtime: picking the one
          the seat is on is then a change, which cancels it. */}
      <PromptInputSelect
        onOpenChange={(next) => {
          if (next && failed) setFailureSeen(failed.id);
          onOpenChange(next);
        }}
        onValueChange={(v) => void assign(v)}
        open={open}
        value={snap.switching?.modelId ?? snap.assignedModelId ?? ""}
      >
        <PromptInputSelectTrigger
          aria-label="Model assigned to this agent"
          className={SEAT_TRIGGER}
          data-testid="assign-model"
          title={
            waiting
              ? `Switches to ${waiting.to} ${waiting.when}; this turn runs on as it is`
              : (assigned?.label ?? snap.assignedModelId ?? "unassigned")
          }
        >
          <span className="truncate" data-testid="agent-model">
            {assigned ? shortRuntime(assigned.label) : (snap.assignedModelId ?? "unassigned")}
          </span>
          {waiting && (
            <span className="shrink-0 text-(--accent)" data-testid="switch-waiting">
              → {waiting.to}
            </span>
          )}
        </PromptInputSelectTrigger>
        <PromptInputSelectContent position="popper">
          {snap.availableModels.map((m) => (
            <PromptInputSelectItem key={m.id} value={m.id}>
              {m.label}
            </PromptInputSelectItem>
          ))}
          {/* An assigned runtime no longer ready on this socket is still the
              truth about this seat, so it stays listed. */}
          {snap.assignedModelId != null && !assigned && (
            <PromptInputSelectItem value={snap.assignedModelId}>{snap.assignedModelId} (not ready)</PromptInputSelectItem>
          )}
        </PromptInputSelectContent>
      </PromptInputSelect>
      {(error || failure) && (
        <span className="text-destructive text-xs" data-testid="seat-error">
          {error || failure}
        </span>
      )}
    </>
  );
}

/** The last instruction you sent in this conversation (↑ recalls it). */
function lastSent(items: readonly TranscriptItem[]): string {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.kind === "human") return item.text;
  }
  return "";
}

const MODE_LABEL: Record<OperatingMode, string> = {
  manual: "Manual",
  plan: "Plan",
  auto_edit: "Edit automatically",
  auto: "Auto",
};

const MODE_DESCRIPTION: Record<OperatingMode, string> = {
  manual: "ask before edits",
  plan: "explain and read only",
  auto_edit: "files only",
  auto: "commands and granted capabilities",
};

/** ACP deliberately leaves provider-defined categories open-ended. Context
 * controls are rendered only when the adapter explicitly categorizes one as
 * a context-size/configuration control; a label that merely happens to say
 * "context" is not enough to invent a writable selector. */
function isNativeContextOption(option: { category?: string }): boolean {
  return ["context", "context_size", "context_window", "context_length"].includes(
    option.category?.toLowerCase() ?? "",
  );
}

/** A capability label comes from the account adapter's catalog — never from
 * a provider-name lookup table. Keep the compact form familiar from the
 * composer while preserving a non-round-million value. */
function contextCapabilityLabel(contextWindow: number | null | undefined): string | null {
  if (typeof contextWindow !== "number" || !Number.isFinite(contextWindow) || contextWindow <= 0) return null;
  if (contextWindow >= 1_000_000) {
    const millions = contextWindow / 1_000_000;
    return `${Number.isInteger(millions) ? millions : millions.toFixed(1)}M context`;
  }
  if (contextWindow >= 1_000) return `${Math.round(contextWindow / 1_000)}k context`;
  return `${contextWindow.toLocaleString()} tokens`;
}

/** What became of one write of a runtime setting: the runtime took it and
 *  answered with its settings, it refused (or could not be asked), or a
 *  newer read of the runtime got in first so the answer went unused. */
type NativeWrite =
  | { kind: "acknowledged"; runtime: AcpRuntimeSnapshot }
  | { kind: "refused" }
  | { kind: "overtaken" };
const REFUSED: NativeWrite = { kind: "refused" };
const OVERTAKEN: NativeWrite = { kind: "overtaken" };

/** Rejecting with this tells upstream's `PromptInput` to leave the textarea
 *  alone, which is the only way to stop it clearing the box itself.
 *
 *  It clears on RESOLVE, and it clears whatever is in the box at that
 *  moment — not what was submitted. A queue acknowledgement is quick, but a
 *  file upload or stale panel can still overlap newer typing, so resolving
 *  would delete an unrelated draft. This component therefore clears the
 *  exact revision it submitted, at submit time, and always rejects. Upstream
 *  swallows the rejection by design ("Don't clear on error"). */
const COMPOSER_OWNS_CLEAR = new Error("composer clears its own input");

function recoverableDraft(submitted: string, current: string): string {
  if (submitted === "") return current;
  if (current.trim() === "" || current === submitted) return submitted;
  if (current.startsWith(`${submitted}\n\n`)) return current;
  return `${submitted}\n\n${current}`;
}

/** Catalog refreshes can change a model's effort vocabulary: never send an
 * effort that belongs to the previously picked model. The model itself
 * stays, listed or not: an exact id the operator typed is theirs to try (the
 * provider says so if it's wrong). An unavailable catalog is not evidence
 * that a last-known-good choice became invalid, so it is left intact. */
function normalizeHttpOverride(
  catalog: ModelCatalog,
  assignedModelId: string,
  value: HttpTurnOverride,
): HttpTurnOverride {
  if (catalog.status !== "available") return value;
  const efforts = effortLevelsForOverride(catalog, assignedModelId, value.modelId);
  return {
    modelId: value.modelId,
    effort: value.effort !== "" && efforts.some((effort) => effort.id === value.effort) ? value.effort : "",
  };
}

function safeStorage() {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    // A browser configured to throw on storage access. Drafts degrade to
    // in-memory; the composer still works.
    return null;
  }
}

export type ComposerProps = {
  snap: AgentPanelSnapshot;
  actions: AgentPanelActions;
  cancellable: boolean;
  /** The node accepted an instruction from this composer. */
  onSent: () => void;
  /** Filled with this composer's own attach function, so files dropped
   *  anywhere on the panel land here, under the same checks as the picker. */
  attachRef?: MutableRefObject<((files: File[]) => void) | null>;
  /** Filled with this composer's take-back, so the queue's Edit lands here
   *  the way ↑ does. */
  takeBackRef?: MutableRefObject<((item: QueuedItem) => void) | null>;
  /** "● N agents": what this agent started, from the shell. */
  agents?: ReactNode;
};

export function Composer(props: ComposerProps) {
  const storage = useMemo(() => safeStorage(), []);
  const drafts = useMemo(() => new Draftbox(storage), [storage]);
  const mountKey = useRef({
    streamId: props.snap.streamId,
    agentId: props.snap.agentId,
  });
  // Read once, at mount: this seeds the provider's initial value. Every
  // later write goes through the effect in `ComposerInner`.
  const initial = useMemo(() => drafts.read(mountKey.current.streamId, mountKey.current.agentId), [drafts]);
  const attachmentDraftKey = `bakery.attachment-draft.v1:${mountKey.current.streamId ?? "-"}:${mountKey.current.agentId}`;

  return (
    <PromptInputProvider attachmentDraftKey={attachmentDraftKey} initialInput={initial}>
      <ComposerInner {...props} drafts={drafts} />
    </PromptInputProvider>
  );
}

function ComposerInner({
  snap,
  actions,
  cancellable,
  onSent,
  attachRef,
  takeBackRef,
  agents,
  drafts,
}: ComposerProps & {
  drafts: Draftbox;
}) {
  const controller = usePromptInputController();
  const attachments = usePromptInputAttachments();
  // The provider's add applies the same type/size/count checks as the picker.
  useEffect(() => {
    if (!attachRef) return;
    attachRef.current = (files) => attachments.add(files);
    return () => {
      attachRef.current = null;
    };
  }, [attachRef, attachments]);
  const text = controller.textInput.value;

  const [error, setError] = useState("");
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [accountCapabilities, setAccountCapabilities] = useState<AccountCapabilities | null>(null);
  const [capabilityIdentity, setCapabilityIdentity] = useState<string | null>(null);
  const [capabilitiesLoading, setCapabilitiesLoading] = useState(snap.assignedNative);
  const [catalogError, setCatalogError] = useState("");
  // On the Bakery's own loop the picker shows the thread's selection; a
  // native seat's comes from its session (`adoptNativeSnapshot`).
  const [overrideModel, setOverrideModel] = useState(
    snap.assignedNative || snap.selection.model === "" ? SEAT_DEFAULT : snap.selection.model,
  );
  const [overrideEffort, setOverrideEffort] = useState(
    snap.assignedNative || snap.selection.effort === "" ? SEAT_DEFAULT : snap.selection.effort,
  );
  const [pickerOpen, setPickerOpen] = useState(false);
  const [menuIndex, setMenuIndex] = useState(0);
  const [menuDismissed, setMenuDismissed] = useState(false);
  // @-mentions: worktree files the message carries, each read and attached on Send.
  const [mentions, setMentions] = useState<string[]>([]);
  const mentionsRef = useRef(mentions);
  mentionsRef.current = mentions;
  const [found, setFound] = useState<string[]>([]);
  const [mentionIndex, setMentionIndex] = useState(0);
  const [mentionDismissed, setMentionDismissed] = useState(false);
  // Controlled so `/model` and `/effort` can actually OPEN them. A command
  // that only warmed a cache and left the human to find the control was an
  // advertised action that did not do the thing it named.
  const [modelOpen, setModelOpen] = useState(false);
  /** What's typed in the model chooser: a search, or an exact model id. */
  const [modelQuery, setModelQuery] = useState("");
  const [seatOpen, setSeatOpen] = useState(false);
  /** Set when a real storage refused the draft. Never silently swallowed:
   *  the human needs to know their words are not safe before they close the
   *  pane. */
  const [draftUnsaved, setDraftUnsaved] = useState(false);
  /** While the agent works, a message steers into its next step (as the
   *  Claude Code extension does); "Later" holds this one for after the turn. */
  const [later, setLater] = useState(false);
  const steerNext = snap.busy && !later;
  const [stopping, setStopping] = useState(false);
  const [resumingDispatch, setResumingDispatch] = useState(false);
  const [submittedDraft, setSubmittedDraft] = useState("");
  const [nativeConfigApplying, setNativeConfigApplying] = useState(false);

  const inFlight = useRef(false);
  const capabilityRequest = useRef(0);
  const nativeCapabilityFlight = useRef<{ identity: string; promise: Promise<boolean> } | null>(null);
  // Session generation identifies a native process, not the ordering of
  // configuration snapshots *within* that process. Every fetch/mutation gets
  // one local order so a delayed same-generation response cannot roll back a
  // newer acknowledged value.
  const nativeSnapshotOrder = useRef(0);
  const nativeConfigRequest = useRef(0);
  const nativeConfigInFlight = useRef(false);
  // An effort pick moves its control at once and reaches the runtime in the
  // background (`EffortPicks`, with the hands given below). The effort the
  // runtime last answered with is what a refused pick goes back to: `null`
  // when it reports none, `undefined` until it has answered.
  const acknowledgedEffort = useRef<string | null | undefined>(undefined);
  const effortHands = useRef<Pick<EffortHost, "show" | "write" | "read">>({
    show: () => {},
    write: async () => "refused",
    read: async () => {},
  });
  const effortPicks = useRef<EffortPicks | null>(null);
  effortPicks.current ??= new EffortPicks({
    show: (value) => effortHands.current.show(value),
    write: (value) => effortHands.current.write(value),
    read: () => effortHands.current.read(),
    acknowledged: () => acknowledgedEffort.current,
  });
  const newestNativeGeneration = useRef<{ runtime: string; generation: number } | null>(null);
  const identity = `${snap.streamId ?? "-"}\u0000${snap.agentId}\u0000${snap.assignedModelId ?? "-"}`;
  const identityRef = useRef(identity);
  identityRef.current = identity;
  const nativeCapabilitiesKnown = capabilityIdentity === identity;
  const currentAccountCapabilities = nativeCapabilitiesKnown ? accountCapabilities : null;
  const pendingKey = pendingTurnKey(snap.streamId, snap.agentId);
  const subscribePending = useCallback(
    (notify: () => void) => subscribePendingTurn(pendingKey, notify),
    [pendingKey],
  );
  const readPending = useCallback(() => hasPendingTurn(pendingKey), [pendingKey]);
  const pendingSubmission = useSyncExternalStore(subscribePending, readPending, () => false);
  const nativeRef = useRef(snap.assignedNative);
  nativeRef.current = snap.assignedNative;
  const steerRef = useRef(steerNext);
  steerRef.current = steerNext;

  // ---- drafts ------------------------------------------------------------
  // Debounced so a long paste is not rewritten on every keystroke, plus an
  // unmount flush so closing a pane mid-sentence still keeps the words.
  const latestText = useRef(text);
  latestText.current = text;
  const submittedDraftRef = useRef(submittedDraft);
  submittedDraftRef.current = submittedDraft;
  const mounted = useRef(true);
  // The controller too: `submit` and `restoreInstruction` both act after an
  // await, when their render-time closures are stale.
  const controllerRef = useRef(controller);
  controllerRef.current = controller;
  const draftKeyRef = useRef({ streamId: snap.streamId, agentId: snap.agentId });
  draftKeyRef.current = { streamId: snap.streamId, agentId: snap.agentId };

  useEffect(() => {
    const id = setTimeout(() => {
      // `failed` means a real storage rejected it — the draft is whole in
      // memory but will not survive closing this pane. Say so.
      setDraftUnsaved(
        drafts.write(snap.streamId, snap.agentId, recoverableDraft(submittedDraft, text)) === "failed",
      );
    }, 200);
    return () => clearTimeout(id);
  }, [drafts, snap.agentId, snap.streamId, submittedDraft, text]);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      const { streamId, agentId } = draftKeyRef.current;
      drafts.write(streamId, agentId, recoverableDraft(submittedDraftRef.current, latestText.current));
    };
  }, [drafts]);

  useEffect(() => {
    const streamId = snap.streamId;
    const agentId = snap.agentId;
    const initialBoundText = latestText.current;
    const sync = ({ previous, next }: { previous: string | null; next: string | null }) => {
      const before = previous ?? "";
      const after = next ?? "";
      const live = latestText.current;
      // Apply an external settlement only while this input still displays
      // the revision that changed. Unsaved keystrokes are newer and win.
      if (live !== before && live !== after) return;
      if (live === after) return;
      latestText.current = after;
      controllerRef.current.textInput.setInput(after);
    };
    const unsubscribe = drafts.subscribe(streamId, agentId, sync);
    // Close mount -> effect races just like the attachment store does.
    const persisted = drafts.read(streamId, agentId);
    if (latestText.current === initialBoundText && persisted !== initialBoundText) {
      latestText.current = persisted;
      controllerRef.current.textInput.setInput(persisted);
    }
    return unsubscribe;
  }, [drafts, snap.agentId, snap.streamId]);

  // The thread's selection, wherever it changed (this pane, another, an
  // agent's thread_create), is what the picker shows.
  useEffect(() => {
    if (snap.assignedNative) return;
    setOverrideModel(snap.selection.model === "" ? SEAT_DEFAULT : snap.selection.model);
    setOverrideEffort(snap.selection.effort === "" ? SEAT_DEFAULT : snap.selection.effort);
  }, [snap.assignedNative, snap.selection.model, snap.selection.effort]);

  // ---- model / effort catalog -------------------------------------------
  // Lazy by contract: fetched only when a picker is actually opened, and
  // dropped when the seat's assignment changes underneath it.
  useEffect(() => {
    capabilityRequest.current += 1;
    nativeCapabilityFlight.current = null;
    nativeSnapshotOrder.current += 1;
    nativeConfigRequest.current += 1;
    nativeConfigInFlight.current = false;
    // An effort still on its way belonged to the old session: its answer
    // counts for nothing, and a pick made now is sent at once.
    effortPicks.current?.reset();
    acknowledgedEffort.current = undefined;
    // Switching from an HTTP provider to a native assignment starts in an
    // unknown/loading state. Treating the short gap before the capability
    // request as "unsupported" would let PromptInput prune compatible draft
    // attachments before the adapter had answered.
    setCapabilitiesLoading(snap.assignedNative);
    setNativeConfigApplying(false);
    setCatalog(null);
    setAccountCapabilities(null);
    setCapabilityIdentity(null);
    setCatalogError("");
    setModelOpen(false);
    if (snap.assignedNative) {
      setOverrideModel(SEAT_DEFAULT);
      setOverrideEffort(SEAT_DEFAULT);
    }
    newestNativeGeneration.current = null;
  }, [snap.agentId, snap.assignedModelId, snap.assignedNative, snap.streamId]);

  useEffect(
    () => () => {
      // A disposed pane must be unable to commit a late catalog/config result
      // into a React tree that no longer owns the request.
      capabilityRequest.current += 1;
      nativeSnapshotOrder.current += 1;
      nativeConfigRequest.current += 1;
      nativeConfigInFlight.current = false;
      effortPicks.current?.reset();
    },
    [],
  );

  // A pick is the thread's selection on the node: every turn runs on it,
  // whoever sends, until changed, and the thread shows the change.
  const persistHttpOverride = useCallback(
    (model: string, effort: string) => {
      if (snap.assignedNative) return;
      const chosenModel = model === SEAT_DEFAULT ? "" : model;
      const chosenEffort = effort === SEAT_DEFAULT ? "" : effort;
      const models = catalog?.status === "available" ? catalog.models : [];
      const modelLabel = models.find((m) => m.id === chosenModel)?.label;
      const effortLabel = effortLevelsForOverride(catalog, snap.assignedModelId, chosenModel).find((l) => l.id === chosenEffort)?.label;
      void actions.select({ model: chosenModel, effort: chosenEffort, modelLabel, effortLabel }).then((out) => {
        if (out.status === "error") setError(out.error);
      });
    },
    [actions, catalog, snap.assignedModelId, snap.assignedNative],
  );

  const applyHttpOverride = useCallback(
    (model: string, effort: string) => {
      setOverrideModel(model);
      setOverrideEffort(effort);
      persistHttpOverride(model, effort);
    },
    [persistHttpOverride],
  );

  const adoptNativeSnapshot = useCallback((runtime: AcpRuntimeSnapshot, allowFreshFetch = false) => {
    const current = newestNativeGeneration.current;
    // A config mutation response from an older session must never roll the
    // picker backward. A *newest* capability fetch is different: its request
    // counter below already fences older HTTP responses, and it may be the
    // first observation after the node itself restarted (where a fresh
    // process generation legitimately starts below the old process's value).
    if (!allowFreshFetch && current && current.runtime === runtime.account_id && runtime.generation < current.generation) {
      return false;
    }
    if (!current || current.runtime !== runtime.account_id || current.generation !== runtime.generation) {
      // Any mutation started against the prior native process is stale even
      // if its HTTP response happens to arrive after this fresher snapshot.
      nativeConfigRequest.current += 1;
      nativeConfigInFlight.current = false;
      setNativeConfigApplying(false);
    }
    newestNativeGeneration.current = { runtime: runtime.account_id, generation: runtime.generation };
    const model = runtime.config_options.find((option) => option.category === "model")?.current_value;
    const effort = runtime.config_options.find((option) => option.category === "thought_level")?.current_value;
    acknowledgedEffort.current = typeof effort === "string" && effort !== "" ? effort : null;
    setOverrideModel(typeof model === "string" && model !== "" ? model : SEAT_DEFAULT);
    setOverrideEffort(typeof effort === "string" && effort !== "" ? effort : SEAT_DEFAULT);
    return true;
  }, []);

  const fetchNativeCapabilities = useCallback((force = false): Promise<boolean> => {
    const assignmentId = snap.assignedModelId;
    if (!snap.assignedNative || assignmentId == null) return Promise.resolve(false);
    const requestIdentity = identityRef.current;
    const existing = nativeCapabilityFlight.current;
    if (!force && existing?.identity === requestIdentity) return existing.promise;
    const request = ++capabilityRequest.current;
    const snapshotOrder = ++nativeSnapshotOrder.current;
    // A newer full snapshot supersedes an older in-flight mutation response,
    // even when both belong to the same native session generation.
    nativeConfigRequest.current += 1;
    nativeConfigInFlight.current = false;
    setNativeConfigApplying(false);
    const flight = { identity: requestIdentity, promise: Promise.resolve(false) };
    setCapabilitiesLoading(true);
    const promise = (async () => {
      try {
        const out = await actions.fetchAccountCapabilities(assignmentId);
        if (
          request !== capabilityRequest.current ||
          snapshotOrder !== nativeSnapshotOrder.current ||
          requestIdentity !== identityRef.current
        ) return false;
        if (out.status === "ok") {
          if (out.value.native_runtime) {
            if (!adoptNativeSnapshot(out.value.native_runtime, true)) return false;
          } else {
            // This is the newest response for this exact assignment. An
            // unavailable/restarting native session must clear the last
            // generation and its model/effort rather than leave stale controls
            // visible (or worse, submitted as though still acknowledged).
            nativeConfigRequest.current += 1;
            nativeConfigInFlight.current = false;
            newestNativeGeneration.current = null;
            setNativeConfigApplying(false);
            setOverrideModel(SEAT_DEFAULT);
            setOverrideEffort(SEAT_DEFAULT);
          }
          setCapabilityIdentity(requestIdentity);
          setAccountCapabilities(out.value);
          setCatalog(out.value.catalog);
          setCatalogError(
            out.value.availability_reason ??
              (out.value.catalog.status === "unavailable" ? out.value.catalog.reason : ""),
          );
          return true;
        }
        if (out.status === "error") setCatalogError(out.error);
        return out.status !== "stale";
      } finally {
        if (nativeCapabilityFlight.current === flight) nativeCapabilityFlight.current = null;
        if (request === capabilityRequest.current && requestIdentity === identityRef.current) {
          setCapabilitiesLoading(false);
        }
      }
    })();
    flight.promise = promise;
    nativeCapabilityFlight.current = flight;
    return promise;
  }, [actions, adoptNativeSnapshot, snap.assignedModelId, snap.assignedNative]);

  // Native prompt types, modes, commands and current configuration are
  // session facts, so initialize the adapter when its pane mounts.
  // HTTP catalogs retain their prior lazy-on-picker behavior.
  useEffect(() => {
    if (!snap.assignedNative) return;
    void fetchNativeCapabilities();
  }, [fetchNativeCapabilities, snap.assignedNative]);

  // A reconnect can mean the node restarted and this agent's native session
  // died with it: refresh its models/effort/context now, instead of keeping
  // the dead session's snapshot (or a startup error) until a picker opens.
  const seenReconnects = useRef(snap.reconnects);
  useEffect(() => {
    if (seenReconnects.current === snap.reconnects) return;
    seenReconnects.current = snap.reconnects;
    void fetchNativeCapabilities(true);
  }, [fetchNativeCapabilities, snap.reconnects]);

  const ensureCatalog = useCallback(async (): Promise<boolean> => {
    if (snap.assignedModelId == null) return false;
    if (snap.assignedNative) {
      // Native configuration can change outside this projection through an
      // adapter notification, another client, or a respawn. Opening a
      // control is an explicit refresh point; the in-flight helper above
      // deduplicates model/effort/context opening together.
      return fetchNativeCapabilities();
    }
    if (catalog !== null) return true;
    const request = ++capabilityRequest.current;
    const requestIdentity = identityRef.current;
    const assignmentId = snap.assignedModelId;
    setCapabilitiesLoading(true);
    try {
      const out = await actions.fetchModelCatalog(assignmentId);
      if (request !== capabilityRequest.current || requestIdentity !== identityRef.current) return false;
      if (out.status === "ok") {
        setCatalog(out.value);
        setCatalogError(out.value.status === "unavailable" ? out.value.reason : "");
        const current = {
          modelId: overrideModel === SEAT_DEFAULT ? "" : overrideModel,
          effort: overrideEffort === SEAT_DEFAULT ? "" : overrideEffort,
        };
        const normalized = normalizeHttpOverride(out.value, assignmentId, current);
        // Saved only when the catalog no longer offers the choice.
        if (normalized.modelId !== current.modelId || normalized.effort !== current.effort) {
          applyHttpOverride(
            normalized.modelId === "" ? SEAT_DEFAULT : normalized.modelId,
            normalized.effort === "" ? SEAT_DEFAULT : normalized.effort,
          );
        }
        return true;
      }
      if (out.status === "error") setCatalogError(out.error);
      return out.status !== "stale";
    } finally {
      if (request === capabilityRequest.current && requestIdentity === identityRef.current) {
        setCapabilitiesLoading(false);
      }
    }
  }, [
    actions,
    applyHttpOverride,
    catalog,
    fetchNativeCapabilities,
    overrideEffort,
    overrideModel,
    snap.assignedModelId,
    snap.assignedNative,
  ]);

  // A restored non-default choice needs its catalog to recover labels and
  // dependent effort options. Default composers remain lazy; reopening a
  // pane with armed one-turn intent is the one justified eager fetch.
  useEffect(() => {
    if (
      snap.assignedModelId != null &&
      !snap.assignedNative &&
      catalog === null &&
      (overrideModel !== SEAT_DEFAULT || overrideEffort !== SEAT_DEFAULT)
    ) {
      void ensureCatalog();
    }
  }, [catalog, ensureCatalog, overrideEffort, overrideModel, snap.assignedModelId, snap.assignedNative]);

  const nativeRuntime = currentAccountCapabilities?.native_runtime;
  const nativeModelOption = nativeRuntime?.config_options.find(
    (option) => option.category === "model" && option.kind === "select",
  );
  const nativeEffortOption = nativeRuntime?.config_options.find(
    (option) => option.category === "thought_level" && option.kind === "select",
  );
  const nativeContextOptions = (nativeRuntime?.config_options ?? []).filter(
    (option) => option.kind !== "unsupported" && isNativeContextOption(option),
  );
  const overrideModels = nativeModelOption
    ? nativeModelOption.values.map((model) => ({
        id: model.id,
        label: model.name,
        context_window: null,
        effort_levels: [],
      }))
    : !snap.assignedNative && catalog?.status === "available"
      ? catalog.models
      : [];
  const resolvedModelId = overrideModel === SEAT_DEFAULT ? "" : overrideModel;
  const effortLevels: EffortLevel[] = nativeEffortOption
    ? nativeEffortOption.values.map((effort) => ({ id: effort.id, label: effort.name }))
    : effortLevelsForOverride(catalog, snap.assignedModelId, resolvedModelId);
  /** Slash-command availability follows the actual catalog. The toolbar
   *  control itself remains mounted even when enumeration is unavailable:
   *  making the button disappear under the operator's pointer looked like a
   *  broken picker and hid the adapter's useful reason. */
  const overrideMaybeAvailable = catalog === null || overrideModels.length > 0;
  const showModelOverride = snap.assignedModelId != null;
  const assignedCatalogModel = overrideModels.find((m) => m.id === snap.assignedModelId);
  const assignedContextLabel = contextCapabilityLabel(assignedCatalogModel?.context_window);
  const selectedOverrideModel = overrideModels.find((m) => m.id === overrideModel);
  const selectedContextLabel = contextCapabilityLabel(selectedOverrideModel?.context_window);
  const modelOverrideLabel =
    nativeRuntime && overrideModel !== SEAT_DEFAULT
      ? `${selectedOverrideModel?.label ?? overrideModel}${selectedContextLabel ? ` · ${selectedContextLabel}` : ""}`
      : overrideModel === SEAT_DEFAULT
      ? `${assignedCatalogModel?.label ?? snap.assignedModelId} (assigned)${assignedContextLabel ? ` · ${assignedContextLabel}` : ""}`
      : `${selectedOverrideModel?.label ?? overrideModel}${selectedContextLabel ? ` · ${selectedContextLabel}` : ""}`;
  const effortOverrideLabel =
    overrideEffort === SEAT_DEFAULT
      ? "effort: default"
      : (effortLevels.find((l) => l.id === overrideEffort)?.label ?? overrideEffort);

  // Which keys are saved, kept for every composer (the Settings key form
  // refreshes it), so the picker opens on what is already here.
  const keys = useCached(snap.assignedProvider ? "auth-status" : null, () => authStatus(), { maxAge: 5 * 60_000 });
  const providers = providersOffered(keys.data, snap.assignedProvider);
  /** Mid-turn the node keeps the turn running and moves the seat when it
   *  ends; the picker says so until then. */
  const moveToProvider = (provider: string) => {
    if (snap.assignedModelId == null) return;
    setModelOpen(false);
    void actions.assignModel(snap.assignedModelId, provider).then((out) => out.status === "error" && setError(out.error));
  };
  // A switch of provider waiting for this turn to end (another runtime's
  // shows on the seat's pill).
  const providerWaiting = waitingProvider(snap.switching, snap.assignedModelId);
  const switchNote = providerWaiting ? waitingSwitchNote(snap.switching, snap.assignedModelId, snap.busy, (id) => id) : null;

  /** One write of a runtime setting, and what became of it. A quiet write
   *  (an effort) holds nothing back: its control has already moved, and the
   *  picker stays usable while it is sent. */
  const writeNativeConfig = useCallback(
    async (optionId: string, value: string | boolean, quiet = false): Promise<NativeWrite> => {
      // An effort still on its way to the runtime goes first; the controls
      // wait with it, as they do for any other setting.
      const sending = effortPicks.current?.sending;
      if (!quiet && sending) {
        setNativeConfigApplying(true);
        await sending;
        setNativeConfigApplying(false);
      }
      const runtime = currentAccountCapabilities?.native_runtime;
      if (!runtime) {
        setError("The runtime is not ready; reopen the picker to retry setup.");
        return REFUSED;
      }
      if (nativeConfigInFlight.current) return REFUSED;
      nativeConfigInFlight.current = true;
      if (!quiet) setNativeConfigApplying(true);
      const request = ++nativeConfigRequest.current;
      const snapshotOrder = ++nativeSnapshotOrder.current;
      const requestIdentity = identityRef.current;
      const expected = { runtime: runtime.account_id, generation: runtime.generation };
      const out = await actions.setNativeConfig(runtime.generation, optionId, value);
      const current = newestNativeGeneration.current;
      const stillCurrent =
        request === nativeConfigRequest.current &&
        snapshotOrder === nativeSnapshotOrder.current &&
        requestIdentity === identityRef.current &&
        current?.runtime === expected.runtime &&
        current.generation === expected.generation;
      // A newer read of the runtime, or another session, got in first: this
      // answer is not used, and it cannot be said whether the runtime took
      // the write.
      if (!stillCurrent) return OVERTAKEN;
      nativeConfigInFlight.current = false;
      setNativeConfigApplying(false);
      if (out.status === "ok") {
        if (out.value.account_id !== expected.runtime || out.value.generation !== expected.generation) {
          setError("The runtime restarted; its acknowledged settings were kept.");
          return OVERTAKEN;
        }
        if (!adoptNativeSnapshot(out.value)) {
          setError("The runtime restarted; its acknowledged settings were kept.");
          return OVERTAKEN;
        }
        setError("");
        setAccountCapabilities((current) =>
          current == null ? current : { ...current, source: "native_session", native_runtime: out.value },
        );
        return { kind: "acknowledged", runtime: out.value };
      }
      if (out.status === "error") {
        // A native process can restart between reading capabilities and
        // applying one option. The node rejects that generation with 409;
        // refresh once in place so this pane adopts the replacement session
        // without requiring a remount or unrelated mode change.
        if (/stale\s+(?:ACP\s+session\s+)?generation/i.test(out.error)) {
          const refreshed = await fetchNativeCapabilities(true);
          if (!refreshed && requestIdentity === identityRef.current) setError(out.error);
          else if (requestIdentity === identityRef.current) setError("");
        } else {
          setError(out.error);
        }
        return REFUSED;
      }
      // The panel moved to another assignment under the write.
      return OVERTAKEN;
    },
    [actions, adoptNativeSnapshot, currentAccountCapabilities?.native_runtime, fetchNativeCapabilities],
  );

  /** The picker's own writes (the model, a switch, a context choice): the
   *  runtime's answer when it took it, `null` when it did not. */
  const applyNativeConfig = useCallback(
    async (optionId: string, value: string | boolean) => {
      const write = await writeNativeConfig(optionId, value);
      return write.kind === "acknowledged" ? write.runtime : null;
    },
    [writeNativeConfig],
  );

  // What `EffortPicks` does with the composer, as this render has it.
  effortHands.current = {
    show: (value) => {
      const optionId = nativeEffortOption?.id;
      if (optionId !== undefined) {
        setAccountCapabilities((current) =>
          current?.native_runtime
            ? { ...current, native_runtime: withConfigValue(current.native_runtime, optionId, value) }
            : current,
        );
      }
      setOverrideEffort(value ?? SEAT_DEFAULT);
    },
    write: async (value) => {
      if (!nativeEffortOption) return "refused";
      return (await writeNativeConfig(nativeEffortOption.id, value, true)).kind;
    },
    read: async () => void (await fetchNativeCapabilities(true)),
  };
  /** An effort pick: the control moves at once, and the runtime is told in
   *  the background (`EffortPicks`). If it refuses, the control goes back
   *  and the error shows. Nothing in the picker is held meanwhile. */
  const pickEffort = (value: string) =>
    effortPicks.current?.pick(
      value,
      typeof nativeEffortOption?.current_value === "string" ? nativeEffortOption.current_value : null,
    );

  // A setting the runtime never answered (`CONFIG_UNANSWERED` on the node):
  // a restart is the one-click way out when the runtime stays stuck.
  const restartRuntime = useCallback(async () => {
    setError("");
    const out = await actions.restartRuntime();
    if (out.status === "error") setError(out.error);
    else if (out.status === "ok") void fetchNativeCapabilities(true);
  }, [actions, fetchNativeCapabilities]);

  const stopInFlight = useRef(false);
  useEffect(() => {
    if (!cancellable) {
      setStopping(false);
      stopInFlight.current = false;
    }
  }, [cancellable]);

  const stop = useCallback(async () => {
    if (stopInFlight.current) return;
    stopInFlight.current = true;
    setStopping(true);
    setError("");
    const out = await actions.cancel();
    if (out.status === "error") {
      setError(out.error);
      setStopping(false);
      stopInFlight.current = false;
    } else if (out.status === "stale") {
      setStopping(false);
      stopInFlight.current = false;
    } else if (!out.value.cancelled || !cancellable) {
      // A successful Stop always pauses durable dispatch, but there may be no
      // active operation to cancel (including a lagging `busy` projection).
      // In that case no terminal activity push will arrive to settle this
      // local optimistic state, so the acknowledgement is terminal for it.
      setStopping(false);
      stopInFlight.current = false;
    }
    // When an active operation was cancelled, remain in "Stopping…" until
    // the authoritative busy/live projection settles. `cancellable`'s
    // effect above performs that reset. Dispatch pause itself remains a
    // Heart-owned projection and is never synthesized in React.
  }, [actions, cancellable]);

  const resumeQueue = useCallback(async () => {
    if (resumingDispatch) return;
    setResumingDispatch(true);
    setError("");
    const out = await actions.resumeDispatch();
    if (out.status === "error") setError(out.error);
    setResumingDispatch(false);
  }, [actions, resumingDispatch]);

  const resumeInterrupted = useCallback(async () => {
    setError("");
    const out = await actions.resume();
    if (out.status === "error") setError(out.error);
  }, [actions]);

  // ---- slash menu --------------------------------------------------------
  const commands = useMemo(
    () => {
      const bakeryCommands = availableCommands({
        streamId: snap.streamId,
        canSend: snap.canSend,
        assignedModelId: snap.assignedModelId,
        // Optimistic before the first fetch, honest after it — the same
        // rule the picker itself follows. Claiming "this provider does not
        // enumerate model variants" before asking the provider would be a
        // guess dressed as a fact.
        hasOverrideModels: overrideMaybeAvailable,
        hasEffortLevels: catalog === null || effortLevels.length > 0,
        hasWorktree: snap.worktreePath !== "" && snap.worktreeOnDisk,
        interrupted: snap.interrupted !== null,
        cancellable,
        assignableModels: snap.availableModels.length,
      });
      const existing = new Set(bakeryCommands.map((command) => command.name));
      const nativeCommands: SlashCommand[] = (nativeRuntime?.commands ?? [])
        .filter((command) => !existing.has(command.name))
        .map((command) => ({
          id: `native:${command.name}`,
          name: command.name,
          label: command.description || command.name,
          hint: command.input_hint ?? "Command the runtime offers",
          kind: "argument",
          disabledReason: snap.canSend ? null : "this native agent is not ready",
        }));
      return [...bakeryCommands, ...nativeCommands];
    },
    [
      cancellable,
      catalog,
      effortLevels.length,
      nativeRuntime?.commands,
      overrideMaybeAvailable,
      snap.assignedModelId,
      snap.availableModels.length,
      snap.canSend,
      snap.interrupted,
      snap.streamId,
      snap.worktreeOnDisk,
      snap.worktreePath,
    ],
  );

  const slash = parseSlash(text);
  const slashQuery = slash?.query;
  const matches = useMemo(
    () => (slashQuery === undefined ? [] : filterCommands(commands, slashQuery)),
    [commands, slashQuery],
  );
  const menuOpen = slashQuery !== undefined && !menuDismissed && matches.length > 0;

  useEffect(() => {
    setMenuIndex(0);
    setMenuDismissed(false);
  }, [slashQuery]);

  // "@name" at the end of the box looks this worktree's files up by name.
  const mentionQuery = /(?:^|\s)@([^\s@]+)$/.exec(text)?.[1];
  const searchIn = snap.streamId;
  useEffect(() => {
    setMentionIndex(0);
    setMentionDismissed(false);
    if (!mentionQuery || !searchIn) {
      setFound([]);
      return;
    }
    let live = true;
    const timer = setTimeout(() => {
      search({ q: mentionQuery, scope: "workspace", stream_id: searchIn }).then(
        (r) => live && setFound([...new Set(r.matches.filter((m) => m.line == null).map((m) => m.path))]),
        () => live && setFound([]),
      );
    }, 150);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [mentionQuery, searchIn]);
  const mentionOpen = !menuOpen && !!mentionQuery && !mentionDismissed && found.length > 0;
  const pickMention = useCallback(
    (path: string) => {
      // A function replacer, so a path holding "$&" or "$1" lands as written.
      controller.textInput.setInput(text.replace(/@([^\s@]+)$/, () => `@${path} `));
      setMentions((m) => (m.includes(path) ? m : [...m, path]));
    },
    [controller, text],
  );

  const runCommand = useCallback(
    (cmd: SlashCommand) => {
      if (cmd.disabledReason) return;
      if (cmd.id.startsWith("native:")) {
        // ACP commands are prompt syntax owned by the adapter. Selecting
        // one inserts the exact advertised command for the operator to add
        // arguments/review before Send; Bakery never invents command argv.
        controller.textInput.setInput(`/${cmd.name}${cmd.hint ? " " : ""}`);
        return;
      }
      controller.textInput.clear();
      switch (cmd.id) {
        case "review":
          actions.openReview();
          break;
        case "resume":
          void resumeInterrupted();
          break;
        case "stop":
          void stop();
          break;
        case "file":
          setPickerOpen(true);
          break;
        case "plan":
          void actions.setOperatingMode("plan").then((out) => {
            if (out.status === "error") setError(out.error);
          });
          break;
        case "model":
          // Fetch AND open. A command that only warmed a cache would be an
          // advertised action that does not do what it says.
          void ensureCatalog().then((current) => {
            if (current) setModelOpen(true);
          });
          break;
        case "effort":
          // Effort is picked in the model pill's own chooser.
          void ensureCatalog().then((current) => {
            if (current) setModelOpen(true);
          });
          break;
        case "assign":
          // The seat pill: reachable for an agent WITH history, not only
          // from an empty-state card that such an agent never shows.
          setSeatOpen(true);
          break;
      }
    },
    [actions, controller.textInput, ensureCatalog, resumeInterrupted, stop],
  );

  /** A waiting message taken back to edit (↑, or Edit in the queue): its
   *  words land in the box, its file in the file chip, and its attachments
   *  stay with it, so sending re-queues all of it the way it was (a
   *  follow-up waits again, a steer steers). Emptying the box drops them:
   *  the message is cancelled. */
  const [carried, setCarried] = useState<string[]>([]);
  const carriedRef = useRef(carried);
  carriedRef.current = carried;
  const takingBack = useRef(false);
  const takeBack = useCallback(
    async (item: QueuedItem) => {
      if (takingBack.current) return;
      takingBack.current = true;
      const out = await actions.withdrawQueued(item.id);
      takingBack.current = false;
      if (out.status === "error") return setError(out.error);
      if (out.status === "stale" || !mounted.current) return;
      setError("");
      controllerRef.current.textInput.setInput(recoverableDraft(out.value.instruction, latestText.current));
      if (out.value.filePath) actions.setSelectedFile(out.value.filePath);
      setCarried((ids) => [...ids, ...out.value.attachmentIds]);
      setLater(item.kind === "follow_up");
    },
    [actions],
  );
  useEffect(() => {
    if (!takeBackRef) return;
    takeBackRef.current = (item) => void takeBack(item);
    return () => {
      takeBackRef.current = null;
    };
  }, [takeBackRef, takeBack]);
  const hadText = useRef(text !== "");
  useEffect(() => {
    if (hadText.current && text === "" && !inFlight.current) setCarried([]);
    hadText.current = text !== "";
  }, [text]);

  /** Capture-phase so the menu sees keys before upstream's textarea handler,
   *  without replacing it. Only the keys the menu actually consumes are
   *  stopped; everything else (Shift+Enter, IME composition, Backspace)
   *  reaches upstream untouched. */
  const onKeyDownCapture = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.nativeEvent.isComposing) return;
      // Keys pressed in a dialog or popover opened from here (the model
      // picker, the command dialog) bubble through React's tree past the
      // portal; they belong to that dialog, not to the composer's shortcuts.
      if (!fromComposer(e.currentTarget, e.target)) return;
      if (mentionOpen) {
        if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          setMentionDismissed(true);
          return;
        }
        if (e.key === "ArrowDown" || e.key === "ArrowUp") {
          e.preventDefault();
          e.stopPropagation();
          setMentionIndex((i) => (i + (e.key === "ArrowDown" ? 1 : found.length - 1)) % found.length);
          return;
        }
        if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
          e.preventDefault();
          e.stopPropagation();
          pickMention(found[Math.min(mentionIndex, found.length - 1)]);
          return;
        }
      }
      if (!menuOpen) {
        // Esc stops the running agent — the Stop button's own action.
        if (e.key === "Escape" && cancellable) {
          e.preventDefault();
          e.stopPropagation();
          void stop();
          return;
        }
        // ↑ in an empty box takes back the newest waiting message to edit,
        // or with none waiting brings back what you last sent.
        if (e.key === "ArrowUp" && text === "" && !e.shiftKey && !e.altKey && !e.metaKey && !e.ctrlKey) {
          const waiting = snap.queuedItems.at(-1);
          if (waiting) {
            e.preventDefault();
            void takeBack(waiting);
            return;
          }
          const last = lastSent(snap.items);
          if (last) {
            e.preventDefault();
            controller.textInput.setInput(last);
          }
        }
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        setMenuDismissed(true);
        return;
      }
      if (matches.length === 0) return;
      if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        setMenuIndex((i) => (i + 1) % matches.length);
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        setMenuIndex((i) => (i - 1 + matches.length) % matches.length);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        e.stopPropagation();
        runCommand(matches[Math.min(menuIndex, matches.length - 1)]);
      }
    },
    [matches, menuIndex, menuOpen, runCommand, cancellable, stop, text, snap.items, snap.queuedItems, takeBack, controller, mentionOpen, found, mentionIndex, pickMention],
  );

  // ---- submit ------------------------------------------------------------

  /** Puts a refused instruction back without ever destroying newer words.
   *
   *  Reads the live text through a ref, not the render closure: a turn can
   *  take minutes, so the value captured when `submit` was created is long
   *  stale by the time this runs. */
  const restoreInstruction = useCallback(
    (instruction: string, submission: { streamId: string | null; agentId: string }) => {
      const restored = recoverableDraft(instruction, latestText.current);
      // Storage is the important half when the pane closed during upload or
      // send: setting state on an unmounted provider cannot recover anything.
      drafts.restoreSubmitted(submission.streamId, submission.agentId, instruction);
      submittedDraftRef.current = "";
      latestText.current = restored;
      if (
        mounted.current &&
        draftKeyRef.current.streamId === submission.streamId &&
        draftKeyRef.current.agentId === submission.agentId
      ) {
        setSubmittedDraft("");
        controllerRef.current.textInput.setInput(restored);
      }
    },
    [drafts],
  );

  // Nothing gates a steer. It carries its text, its attachments, and the
  // selected file (as an attachment) on every runtime. An HTTP seat's
  // one-turn model/effort choice cannot change a turn already running, so a
  // steer leaves it untouched for the next message instead (see `submit`).

  const submit = useCallback(
    async (message: PromptInputMessage) => {
      // PromptInput may still be converting Blob URLs after Dockview has
      // disposed this React root. Such a callback is pre-dispatch: it owns no
      // pending lease and must not stage text, consume overrides, upload, or
      // touch the shared draft stores now owned by a reopened projection.
      if (!mounted.current) throw COMPOSER_OWNS_CLEAR;
      const instruction = message.text.trim();
      if (instruction === "" && message.files.length === 0 && carriedRef.current.length === 0) throw COMPOSER_OWNS_CLEAR;
      if (inFlight.current) throw COMPOSER_OWNS_CLEAR;
      if (snap.disabled || !snap.canSend) {
        setError(snap.unavailableReason ?? "this agent cannot take an instruction right now");
        throw COMPOSER_OWNS_CLEAR;
      }
      // The selected workspace file travels as an attachment on a native
      // runtime, and on every steer.
      const fileAsAttachment = snap.selectedFile !== "" && (!!nativeRuntime || steerNext);
      // The worktree files it carries: the selected one, where it travels as
      // an attachment, and every @-mention still in its words.
      const worktreeFiles = [
        ...(fileAsAttachment ? [snap.selectedFile] : []),
        ...mentionsRef.current.filter((p) => message.text.includes(`@${p}`) && !(fileAsAttachment && p === snap.selectedFile)),
      ];

      const lease = claimPendingTurn(pendingKey);
      if (!lease) {
        setError("The previous instruction is still being accepted. You can keep drafting while it settles.");
        throw COMPOSER_OWNS_CLEAR;
      }
      inFlight.current = true;
      const submission = { streamId: snap.streamId, agentId: snap.agentId };
      const submittedSteer = steerNext;
      const submittedCarried = carriedRef.current;
      // PromptInput converts files asynchronously before it reaches us. Keep
      // the current ids so success removes only THIS submission's files; an
      // attachment added while an upload is in flight is a new draft and
      // must survive exactly like newly typed text does.
      const submittedAttachmentIds = message.files.map((file) => file.id);
      // Persist the submitted revision BEFORE clearing it. Until the queue
      // acknowledgement lands it remains recoverable alongside any newer
      // text, including when Dockview disposes this React root mid-request.
      submittedDraftRef.current = instruction;
      setSubmittedDraft(instruction);
      const liveAtDelivery = latestText.current;
      const newerText = liveAtDelivery === message.text ? "" : liveAtDelivery;
      const draftWrite = drafts.write(
        snap.streamId,
        snap.agentId,
        recoverableDraft(instruction, newerText),
      );
      if (draftWrite === "failed") setDraftUnsaved(true);
      const submittedDraftCapture = drafts.capture(snap.streamId, snap.agentId);
      // Clear THIS revision, now. Everything typed from here on is a new
      // draft that must survive whatever the turn does — see
      // COMPOSER_OWNS_CLEAR for why upstream cannot be trusted to do it.
      latestText.current = newerText;
      if (newerText === "") controllerRef.current.textInput.clear();
      else controllerRef.current.textInput.setInput(newerText);
      // Every picked file's bytes, held before the first upload under URLs
      // this send owns: closing the pane or taking a chip off mid-send
      // revokes the draft's URLs, which must not fail a later file.
      const owned: string[] = [];
      try {
        let held: typeof message.files;
        try {
          // Settled, not raced: every read finishes (and lands in `owned`,
          // revoked below) before a failure gives up.
          const reads = await Promise.allSettled(
            message.files.map(async (file) => {
              if (!file.url) return file;
              const url = URL.createObjectURL(await (await fetch(file.url)).blob());
              owned.push(url);
              return { ...file, url };
            }),
          );
          const failed = reads.find((r) => r.status === "rejected");
          if (failed) throw failed.reason;
          held = reads.map((r) => (r as PromiseFulfilledResult<(typeof message.files)[number]>).value);
        } catch (e) {
          setError(`could not read an attached file: ${e instanceof Error ? e.message : String(e)}`);
          restoreInstruction(instruction, submission);
          throw COMPOSER_OWNS_CLEAR;
        }
        const attachmentIds: string[] = [...submittedCarried];
        for (const file of held) {
          const uploaded = await actions.uploadAttachment(file);
          if (uploaded.status === "error") {
            setError(uploaded.error);
            restoreInstruction(instruction, submission);
            throw COMPOSER_OWNS_CLEAR;
          }
          if (uploaded.status === "stale") {
            restoreInstruction(instruction, submission);
            throw COMPOSER_OWNS_CLEAR;
          }
          attachmentIds.push(...uploaded.value.map((ref) => ref.id));
        }
        // A workspace file is a context block for a native runtime (and for
        // any steer), never a request to let that runtime overwrite the file
        // directly. Read through Bakery's jail, then move bounded bytes into
        // the same node-owned attachment store the picker uses.
        for (const path of worktreeFiles) {
          const read = await actions.readFile(path);
          if (read.status === "error") {
            setError(read.error);
            restoreInstruction(instruction, submission);
            throw COMPOSER_OWNS_CLEAR;
          }
          if (read.status === "stale") {
            restoreInstruction(instruction, submission);
            throw COMPOSER_OWNS_CLEAR;
          }
          // A credential file never goes to a model, picked or @-mentioned.
          if (read.value.secret) {
            setError(`${path} holds secrets, so it isn't sent.`);
            restoreInstruction(instruction, submission);
            throw COMPOSER_OWNS_CLEAR;
          }
          const url = URL.createObjectURL(new Blob([read.value.text], { type: "text/plain;charset=utf-8" }));
          try {
            const uploaded = await actions.uploadAttachment({
              url,
              filename: read.value.path.split("/").pop() || "workspace-context.txt",
              mediaType: "text/plain;charset=utf-8",
              sourcePath: read.value.path,
            });
            if (uploaded.status === "error") {
              setError(uploaded.error);
              restoreInstruction(instruction, submission);
              throw COMPOSER_OWNS_CLEAR;
            }
            if (uploaded.status === "stale") {
              restoreInstruction(instruction, submission);
              throw COMPOSER_OWNS_CLEAR;
            }
            attachmentIds.push(...uploaded.value.map((ref) => ref.id));
          } finally {
            URL.revokeObjectURL(url);
          }
        }
        const out = await actions.send(instruction, {
          filePath: steerNext || nativeRuntime ? "" : snap.selectedFile,
          attachmentIds,
          steer: steerNext,
        });
        if (out.status === "error") {
          setError(out.error);
          restoreInstruction(instruction, submission);
        } else if (out.status === "stale") {
          // The panel rebound mid-flight, so whether the node took it is
          // genuinely unknown. Giving the words back is the conservative
          // half of that ambiguity.
          restoreInstruction(instruction, submission);
        } else {
          setError("");
          submittedDraftRef.current = "";
          setSubmittedDraft("");
          drafts.consumeSubmitted(snap.streamId, snap.agentId, instruction, submittedDraftCapture);
          if (mounted.current && steerRef.current === submittedSteer) setLater(false);
          for (const id of submittedAttachmentIds) attachments.remove(id);
          if (mounted.current) setMentions((m) => m.filter((p) => !worktreeFiles.includes(p)));
          if (mounted.current) setCarried((ids) => (ids === submittedCarried ? [] : ids));
          if (mounted.current) onSent();
        }
      } finally {
        for (const url of owned) URL.revokeObjectURL(url);
        inFlight.current = false;
        releasePendingTurn(lease);
      }
      // Always. Resolving would hand upstream the current box — which by now
      // may hold a completely different draft — and let it delete that.
      throw COMPOSER_OWNS_CLEAR;
    },
    [
      actions,
      attachments,
      drafts,
      nativeRuntime,
      onSent,
      pendingKey,
      restoreInstruction,
      snap.canSend,
      snap.disabled,
      snap.agentId,
      snap.selectedFile,
      snap.streamId,
      snap.unavailableReason,
      steerNext,
    ],
  );

  const rootRef = useRef<HTMLDivElement>(null);

  // Coming back to a thread (a tab, a page, a remount), the draft is picked
  // up where it ends: the caret after its last word and the box scrolled to
  // its last line. The box grows to a height and then scrolls inside, and
  // nothing else keeps that. Before the first paint, so it never shows from
  // the top.
  useLayoutEffect(() => {
    const box = rootRef.current?.querySelector("textarea");
    if (box) toEndOfDraft(box);
  }, []);
  // A pane that is hidden and shown again keeps this tree but not the box's
  // scroll: when the box comes back into view it is put at its end again,
  // unless it is being typed in.
  useEffect(() => {
    const box = rootRef.current?.querySelector("textarea");
    if (!box || typeof IntersectionObserver === "undefined") return;
    let hidden = false;
    const watch = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        if (!entry.isIntersecting) hidden = true;
        else {
          if (hidden && document.activeElement !== box) toEndOfDraft(box);
          hidden = false;
        }
      }
    });
    watch.observe(box);
    return () => watch.disconnect();
  }, []);
  const usage = latestUsage(snap.items);
  const gauge =
    activityGauge(snap.activity) ??
    contextGauge(usage) ??
    (nativeRuntime?.usage && nativeRuntime.usage.size > 0
      ? { used: nativeRuntime.usage.used, window: nativeRuntime.usage.size }
      : null);
  const canSubmit =
    !snap.disabled &&
    snap.canSend &&
    !pendingSubmission &&
    (text.trim() !== "" || attachments.files.length > 0);
  const notice = error || snap.notice || catalogError;
  const rootless = snap.worktreePath === "" || !snap.worktreeOnDisk;
  // The four modes are Bakery policy, enforced server-side in the permission
  // callback — every runtime gets all four, whatever native modes (if any)
  // its adapter advertises.
  const modeChoices = Object.keys(MODE_LABEL) as OperatingMode[];
  const typing = text.trim() !== "" || attachments.files.length > 0;
  const stopShown = cancellable || snap.queuedInstructions > 0;
  /** "/" opens the command menu, as typing it does. */
  const openCommands = () => {
    controller.textInput.setInput("/");
    requestAnimationFrame(() => rootRef.current?.querySelector("textarea")?.focus());
  };
  // Attaching is never switched off and never filtered: every runtime takes
  // every file. The node hands each model what it reads as itself (images;
  // PDFs where the provider takes documents), inlines text, and names
  // anything else — so nothing is refused here, and nothing is dropped there.

  return (
    <div
      className="bakery-ai-composer bakery-ai-control-lane"
      data-testid="composer"
      onKeyDownCapture={onKeyDownCapture}
      ref={rootRef}
    >
      {menuOpen && <SlashMenu activeIndex={menuIndex} matches={matches} onPick={runCommand} />}
      {mentionOpen && <MentionMenu activeIndex={mentionIndex} onPick={pickMention} paths={found} />}

      <PromptInput
        className="bakery-ai-prompt"
        maxFileSize={MAX_ATTACHMENT_BYTES}
        multiple
        onError={(issue) => setError(issue.message)}
        onSubmit={submit}
      >
        <PromptInputBody>
          <PromptInputAttachments className="px-0 pb-2 pt-0">
            {(attachment) => <PromptInputAttachment data={attachment} />}
          </PromptInputAttachments>
          {(snap.selectedFile || carried.length > 0 || mentions.length > 0) && (
            <PromptInputHeader>
              {mentions.map((path) => (
                <Badge className="gap-1 font-mono font-normal" data-testid="mention-chip" key={path} variant="secondary">
                  <FileIcon className="size-3" />
                  <span className="max-w-64 truncate">{path}</span>
                  <button
                    aria-label={`Remove ${path}`}
                    className="opacity-60 hover:opacity-100"
                    onClick={() => setMentions((m) => m.filter((p) => p !== path))}
                    type="button"
                  >
                    <XIcon className="size-3" />
                  </button>
                </Badge>
              ))}
              {snap.selectedFile && (
                <Badge className="gap-1 font-mono font-normal" data-testid="file-chip" variant="secondary">
                  <FileIcon className="size-3" />
                  <span className="max-w-64 truncate">{snap.selectedFile}</span>
                  <button
                    aria-label="Remove file context"
                    className="opacity-60 hover:opacity-100"
                    onClick={() => actions.setSelectedFile("")}
                    type="button"
                  >
                    <XIcon className="size-3" />
                  </button>
                </Badge>
              )}
              {carried.length > 0 && (
                <Badge className="gap-1 font-normal" data-testid="carried-chip" variant="secondary">
                  <PaperclipIcon className="size-3" />
                  {carried.length} attachment{carried.length === 1 ? "" : "s"}
                  <button
                    aria-label="Remove the attachments"
                    className="opacity-60 hover:opacity-100"
                    onClick={() => setCarried([])}
                    type="button"
                  >
                    <XIcon className="size-3" />
                  </button>
                </Badge>
              )}
            </PromptInputHeader>
          )}

          <PromptInputTextarea
            disabled={snap.disabled}
            placeholder={
              snap.disabled
                ? "No thread bound to this panel."
                : snap.pendingAsk
                  ? "Answer the questions above, or type to queue a follow-up…"
                  : snap.busy
                    ? "Queue another message…"
                    : `Reply to ${snap.title || snap.roleLabel}`
            }
          />
        </PromptInputBody>

        <PromptInputFooter className="bakery-ai-composer-footer">
          <PromptInputTools className="bakery-ai-composer-tools-primary">
            <PromptInputButton
              aria-label="Upload attachment from computer"
              className={SQUARE}
              data-testid="upload-attachment"
              disabled={snap.disabled}
              onClick={() => attachments.openFileDialog()}
              title="Attach files"
            >
              <PlusIcon className="size-3.5" />
            </PromptInputButton>
            <PromptInputButton
              aria-label="Add a worktree file as context"
              className={SQUARE}
              data-testid="attach-file"
              disabled={rootless}
              onClick={() => setPickerOpen(true)}
              title="Add a worktree file as context"
            >
              <FolderIcon className="size-3.5" />
            </PromptInputButton>
            <PromptInputButton
              aria-label="Commands"
              className={SQUARE}
              data-testid="open-commands"
              disabled={snap.disabled}
              onClick={openCommands}
              title="Commands"
            >
              <SlashIcon className="size-3.5" />
            </PromptInputButton>
            {agents}

            <SeatPill actions={actions} onOpenChange={setSeatOpen} open={seatOpen} snap={snap} />

            {showModelOverride && (
              <ModelSelector
                onOpenChange={(open) => {
                  setModelOpen(open);
                  if (!open) setModelQuery("");
                  if (open) void ensureCatalog();
                  if (open && nativeRuntime) void fetchNativeCapabilities();
                }}
                open={modelOpen}
              >
                <ModelSelectorTrigger asChild>
                  <PromptInputButton
                    className={PILL_TRIGGER}
                    data-testid="override-model"
                    disabled={capabilitiesLoading || nativeConfigApplying}
                    title={
                      capabilitiesLoading
                        ? "Loading models… (the runtime starts first)"
                        : nativeConfigApplying
                          ? "Applying the model change…"
                          : switchNote
                            ? `Switches to ${switchNote.to} ${switchNote.when}; this turn runs on as it is`
                            : "Choose a model for the next turn"
                    }
                    variant="ghost"
                  >
                    <span className="truncate">
                      {modelOverrideLabel}
                      {snap.assignedProvider ? ` · ${snap.assignedProvider}` : ""}
                    </span>
                    {switchNote && (
                      <span className="shrink-0 text-(--accent)" data-testid="switch-waiting">
                        → {switchNote.to} {switchNote.when}
                      </span>
                    )}
                    {effortLevels.length > 0 && (
                      <span className="shrink-0 text-(--fg-dim)" data-testid="override-effort">
                        {effortOverrideLabel}
                      </span>
                    )}
                    <ChevronDownIcon className="size-3 shrink-0" />
                  </PromptInputButton>
                </ModelSelectorTrigger>
                {snap.assignedNative ? (
                  <NativeModelPicker
                    busy={capabilitiesLoading || nativeConfigApplying}
                    loading={capabilitiesLoading}
                    onApply={applyNativeConfig}
                    onClose={() => setModelOpen(false)}
                    onProvider={moveToProvider}
                    provider={snap.assignedProvider}
                    providerWaiting={providerWaiting}
                    providers={providers}
                    turnRunning={snap.busy}
                    onEffort={pickEffort}
                    runtime={nativeRuntime ?? null}
                  />
                ) : (
                <ModelSelectorContent className="max-w-md" title="Choose model">
                  <ModelSelectorInput
                    onValueChange={setModelQuery}
                    placeholder={nativeModelOption ? "Search available models…" : "Search models, or type an exact model id…"}
                    value={modelQuery}
                  />
                  <ModelSelectorList>
                    <ModelSelectorEmpty>
                      {capabilitiesLoading
                        ? "Loading models…"
                        : catalog?.status === "unavailable"
                          ? catalog.reason
                          : "No matching model."}
                    </ModelSelectorEmpty>
                    <ModelSelectorGroup heading={nativeModelOption ? "Runtime" : "Turn override"}>
                      {/* An exact id, listed or not (the /model of a CLI): an
                          HTTP turn takes any id the provider knows. A native
                          session takes only the models it lists. */}
                      {!nativeModelOption && modelQuery.trim() !== "" && !overrideModels.some((m) => m.id === modelQuery.trim()) && (
                        <ModelSelectorItem
                          forceMount
                          onSelect={() => {
                            applyHttpOverride(modelQuery.trim(), SEAT_DEFAULT);
                            setModelOpen(false);
                            setModelQuery("");
                          }}
                          value={`exact model id ${modelQuery.trim()}`}
                        >
                          <CheckIcon className={`size-3.5 ${overrideModel === modelQuery.trim() ? "opacity-100" : "opacity-0"}`} />
                          <ModelSelectorName>
                            Use <span className="font-mono">{modelQuery.trim()}</span>
                          </ModelSelectorName>
                        </ModelSelectorItem>
                      )}
                      {!nativeModelOption && (
                        <ModelSelectorItem
                          onSelect={() => {
                            const allowed = effortLevelsForOverride(catalog, snap.assignedModelId, "");
                            const nextEffort =
                              overrideEffort !== SEAT_DEFAULT &&
                              allowed.some((effort) => effort.id === overrideEffort)
                                ? overrideEffort
                                : SEAT_DEFAULT;
                            applyHttpOverride(SEAT_DEFAULT, nextEffort);
                            // Stays open while there is an effort to pick too.
                            if (effortLevels.length === 0) setModelOpen(false);
                          }}
                          value={`${snap.assignedModelId ?? "assigned"} assigned default`}
                        >
                          <CheckIcon
                            className={`size-3.5 ${overrideModel === SEAT_DEFAULT ? "opacity-100" : "opacity-0"}`}
                          />
                          <ModelSelectorName>{snap.assignedModelId} (assigned)</ModelSelectorName>
                        </ModelSelectorItem>
                      )}
                      {overrideModels.map((model) => {
                        const contextLabel = contextCapabilityLabel(model.context_window);
                        return (
                          <ModelSelectorItem
                            key={model.id}
                            onSelect={() => {
                              if (nativeModelOption) {
                                void applyNativeConfig(nativeModelOption.id, model.id).then((acknowledged) => {
                                  if (acknowledged && effortLevels.length === 0) setModelOpen(false);
                                });
                              } else {
                                const allowed = effortLevelsForOverride(catalog, snap.assignedModelId, model.id);
                                const nextEffort =
                                  overrideEffort !== SEAT_DEFAULT &&
                                  allowed.some((effort) => effort.id === overrideEffort)
                                    ? overrideEffort
                                    : SEAT_DEFAULT;
                                applyHttpOverride(model.id, nextEffort);
                                if (allowed.length === 0) setModelOpen(false);
                              }
                            }}
                            value={`${model.label} ${model.id} ${contextLabel ?? ""}`}
                          >
                            <CheckIcon
                              className={`size-3.5 ${overrideModel === model.id ? "opacity-100" : "opacity-0"}`}
                            />
                            <ModelSelectorName>
                              {model.label}
                              {contextLabel ? ` · ${contextLabel}` : ""}
                            </ModelSelectorName>
                            {model.label !== model.id && (
                              <span className="truncate font-mono text-muted-foreground text-xs">{model.id}</span>
                            )}
                          </ModelSelectorItem>
                        );
                      })}
                      {catalog?.status === "unavailable" && (
                        <ModelSelectorItem disabled value={`catalog unavailable ${catalog.reason}`}>
                          <ModelSelectorName>{catalog.reason}</ModelSelectorName>
                        </ModelSelectorItem>
                      )}
                    </ModelSelectorGroup>
                    {effortLevels.length > 0 && (
                      <ModelSelectorGroup heading="Effort">
                        {!nativeEffortOption && (
                          <ModelSelectorItem
                            disabled={capabilitiesLoading || nativeConfigApplying}
                            onSelect={() => {
                              applyHttpOverride(overrideModel, SEAT_DEFAULT);
                              setModelOpen(false);
                            }}
                            value="effort default"
                          >
                            <CheckIcon
                              className={`size-3.5 ${overrideEffort === SEAT_DEFAULT ? "opacity-100" : "opacity-0"}`}
                            />
                            <ModelSelectorName>effort: default</ModelSelectorName>
                          </ModelSelectorItem>
                        )}
                        {effortLevels.map((l) => (
                          <ModelSelectorItem
                            disabled={capabilitiesLoading || nativeConfigApplying}
                            key={l.id}
                            onSelect={() => {
                              if (nativeEffortOption) void applyNativeConfig(nativeEffortOption.id, l.id);
                              else applyHttpOverride(overrideModel, l.id);
                              setModelOpen(false);
                            }}
                            value={`effort ${l.label} ${l.id}`}
                          >
                            <CheckIcon className={`size-3.5 ${overrideEffort === l.id ? "opacity-100" : "opacity-0"}`} />
                            <ModelSelectorName>{l.label}</ModelSelectorName>
                          </ModelSelectorItem>
                        ))}
                      </ModelSelectorGroup>
                    )}
                  </ModelSelectorList>
                </ModelSelectorContent>
                )}
              </ModelSelector>
            )}

            {/* Context size is only selectable when this exact native session
                advertises a mutable context option. Otherwise the reported
                capacity remains informational in the gauge below. */}
            {nativeContextOptions.map((option) => {
              const current = option.current_value;
              const value = typeof current === "boolean" ? String(current) : typeof current === "string" ? current : "";
              const currentLabel =
                option.kind === "select"
                  ? (option.values.find((choice) => choice.id === current)?.name ?? String(current ?? "default"))
                  : current === true
                    ? "Enabled"
                    : "Disabled";
              return (
                <PromptInputSelect
                  disabled={capabilitiesLoading || nativeConfigApplying || value === ""}
                  key={option.id}
                  onOpenChange={(open) => {
                    if (open) void fetchNativeCapabilities();
                  }}
                  onValueChange={(next) => {
                    void applyNativeConfig(option.id, option.kind === "boolean" ? next === "true" : next);
                  }}
                  value={value}
                >
                  <PromptInputSelectTrigger className={SELECT_TRIGGER} data-testid={`native-context-${option.id}`}>
                    <span>{option.name}: {currentLabel}</span>
                  </PromptInputSelectTrigger>
                  <PromptInputSelectContent position="popper">
                    {option.kind === "select"
                      ? option.values.map((choice) => (
                          <PromptInputSelectItem key={choice.id} value={choice.id}>
                            {choice.name}
                          </PromptInputSelectItem>
                        ))
                      : (
                          <>
                            <PromptInputSelectItem value="true">Enabled</PromptInputSelectItem>
                            <PromptInputSelectItem value="false">Disabled</PromptInputSelectItem>
                          </>
                        )}
                  </PromptInputSelectContent>
                </PromptInputSelect>
              );
            })}

            {/* Rendered only when a provider actually reported a window.
                No cost row: Bakery records no price, and tokenlens would
                invent one for model ids it has never seen. */}
            {gauge && (
              <Context maxTokens={gauge.window} usedTokens={gauge.used}>
                <ContextTrigger className="h-7 px-2 text-xs" data-testid="context-gauge" />
                <ContextContent>
                  <ContextContentHeader />
                  <ContextContentBody className="space-y-1 text-xs">
                    {usage?.modelId && (
                      <div className="flex justify-between gap-3">
                        <span className="text-muted-foreground">Model</span>
                        <span className="font-mono">{usage.modelId}</span>
                      </div>
                    )}
                    {typeof usage?.tokensIn === "number" && (
                      <div className="flex justify-between gap-3">
                        <span className="text-muted-foreground">Input</span>
                        <span className="font-mono">{promptTokens(usage).toLocaleString()}</span>
                      </div>
                    )}
                    {typeof usage?.tokensOut === "number" && (
                      <div className="flex justify-between gap-3">
                        <span className="text-muted-foreground">Output</span>
                        <span className="font-mono">{usage.tokensOut.toLocaleString()}</span>
                      </div>
                    )}
                    <p className="pt-1 text-muted-foreground">Reported by the provider for the most recent turn.</p>
                  </ContextContentBody>
                </ContextContent>
              </Context>
            )}
          </PromptInputTools>

          <PromptInputTools className="bakery-ai-composer-tools-actions">
            {snap.queuedInstructions > 0 && (
              <span className="px-1 text-muted-foreground text-xs" data-testid="queue-count">
                {snap.queuedInstructions} queued
              </span>
            )}
            {snap.dispatchPaused && (
              <PromptInputButton
                aria-label="Resume queued instructions"
                className={PILL}
                data-testid="resume-dispatch"
                disabled={resumingDispatch}
                onClick={() => void resumeQueue()}
                title="Resume queued instructions"
                variant="outline"
              >
                <PlayIcon className="size-3.5" />
                {resumingDispatch ? "Resuming…" : "Resume"}
              </PromptInputButton>
            )}
            {snap.busy && (
              <PromptInputButton
                aria-label="Send after this turn"
                aria-pressed={later}
                className={PILL}
                data-testid="send-later"
                onClick={() => setLater((current) => !current)}
                title={later ? "This message waits for the turn to end" : "Send this message after the turn instead of into it"}
                variant={later ? "default" : "outline"}
              >
                <TimerIcon className="size-3.5" />
                {later ? "After this turn" : "Later"}
              </PromptInputButton>
            )}
            <PromptInputSelect
              onValueChange={(value) => {
                if (value !== "manual" && value !== "plan" && value !== "auto_edit" && value !== "auto") return;
                void actions.setOperatingMode(value).then((out) => {
                  if (out.status === "error") setError(out.error);
                  if (out.status === "ok" && nativeRuntime) void fetchNativeCapabilities();
                });
              }}
              value={snap.operatingMode}
            >
              <PromptInputSelectTrigger className={MODE_TRIGGER} data-testid="operating-mode">
                <ZapIcon className="size-3 fill-current" />
                <span>{MODE_LABEL[snap.operatingMode]}</span>
              </PromptInputSelectTrigger>
              <PromptInputSelectContent position="popper">
                {modeChoices.map((mode) => (
                  <PromptInputSelectItem key={mode} value={mode}>
                    {MODE_LABEL[mode]} — {MODE_DESCRIPTION[mode]}
                  </PromptInputSelectItem>
                ))}
              </PromptInputSelectContent>
            </PromptInputSelect>
            {stopShown && (
              <PromptInputButton
                aria-label="Stop and pause dispatch"
                className={typing ? SQUARE : ACCENT}
                data-testid="stop"
                disabled={stopping || snap.activity?.cancel_requested === true}
                onClick={() => void stop()}
                title={
                  stopping || snap.activity?.cancel_requested
                    ? "Stopping…"
                    : "Stop this turn and pause queued instructions"
                }
              >
                <SquareIcon className="size-3 fill-current" />
              </PromptInputButton>
            )}
            {/* Always rendered — see the Enter-guard note at the top. Hidden,
                not removed, while Stop takes its place and nothing is typed. */}
            <PromptInputSubmit
              className={stopShown && !typing ? "hidden" : ACCENT}
              data-testid="send"
              disabled={!canSubmit}
              title={snap.unavailableReason ?? (steerNext ? "Steer it in at the agent's next step" : snap.busy ? "Send after this turn" : "Send")}
            >
              <ArrowUpIcon className="size-3.5" />
            </PromptInputSubmit>
          </PromptInputTools>
        </PromptInputFooter>
      </PromptInput>

      {notice && (
        <p className="bakery-ai-surface bakery-ai-dock-note text-destructive text-xs" data-testid="composer-notice">
          {notice}
          {/runtime didn't answer/i.test(error) && (
            <button className="ml-2 font-medium underline" onClick={() => void restartRuntime()} type="button">
              Restart runtime
            </button>
          )}
        </p>
      )}
      {draftUnsaved && (
        <p className="bakery-ai-surface bakery-ai-dock-note text-destructive text-xs" data-testid="draft-unsaved">
          This draft is too large for local storage, so it will be lost if you close this pane. Copy it somewhere
          safe, or send it now.
        </p>
      )}

      <FilePicker actions={actions} onOpenChange={setPickerOpen} open={pickerOpen} worktreePath={snap.worktreePath} />
    </div>
  );
}

// ---- slash menu -----------------------------------------------------------

/** Worktree files whose names match an "@" being typed: every one, scrolling. */
function MentionMenu({ paths, activeIndex, onPick }: { paths: string[]; activeIndex: number; onPick: (path: string) => void }) {
  return (
    <div className="bakery-ai-surface mb-1 max-h-60 overflow-auto rounded-md border border-border bg-popover" data-testid="mention-menu" role="listbox">
      {paths.map((path, i) => (
        <button
          aria-selected={i === activeIndex}
          className={`flex w-full items-center gap-2 px-2 py-1.5 text-left text-sm ${i === activeIndex ? "bg-accent" : ""}`}
          key={path}
          onClick={() => onPick(path)}
          role="option"
          type="button"
        >
          <FileIcon className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-mono text-xs">{path}</span>
        </button>
      ))}
    </div>
  );
}

function SlashMenu({
  matches,
  activeIndex,
  onPick,
}: {
  matches: SlashCommand[];
  activeIndex: number;
  onPick: (cmd: SlashCommand) => void;
}) {
  return (
    <div className="bakery-ai-surface mb-1 overflow-hidden rounded-md border border-border bg-popover" data-testid="slash-menu" role="listbox">
      {matches.map((cmd, i) => (
        <button
          aria-selected={i === activeIndex}
          className={`flex w-full items-baseline gap-2 px-2 py-1.5 text-left text-sm ${
            i === activeIndex ? "bg-accent" : ""
          } ${cmd.disabledReason ? "opacity-50" : ""}`}
          disabled={cmd.disabledReason !== null}
          key={cmd.id}
          onClick={() => onPick(cmd)}
          role="option"
          type="button"
        >
          <span className="font-mono">/{cmd.name}</span>
          <span className="text-muted-foreground text-xs">{cmd.disabledReason ?? cmd.hint}</span>
        </button>
      ))}
    </div>
  );
}

// ---- file picker ----------------------------------------------------------

function FilePicker({
  open,
  onOpenChange,
  actions,
  worktreePath,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  actions: AgentPanelActions;
  worktreePath: string;
}) {
  const [dir, setDir] = useState("");
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    setError("");
    void actions.listDir(dir).then((out) => {
      if (cancelled) return;
      setLoading(false);
      if (out.status === "ok") setEntries(out.value);
      // `stale` leaves the last good listing alone rather than blanking it.
      if (out.status === "error") {
        setEntries([]);
        setError(out.error);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [actions, dir, open]);

  const parent = dir === "" ? null : dir.split("/").slice(0, -1).join("/");

  return (
    <CommandDialog
      description={worktreePath || "worktree"}
      onOpenChange={(next) => {
        onOpenChange(next);
        if (!next) setDir("");
      }}
      open={open}
      title="Add a worktree file"
    >
      <CommandInput placeholder={dir === "" ? "Search this worktree…" : `Search ${dir}…`} />
      <CommandList>
        {loading && <div className="p-3 text-muted-foreground text-sm">Loading…</div>}
        {error && <div className="p-3 text-destructive text-sm">{error}</div>}
        {!loading && !error && <CommandEmpty>Nothing here.</CommandEmpty>}
        <CommandGroup heading={dir === "" ? "/" : `/${dir}`}>
          {parent !== null && (
            <CommandItem onSelect={() => setDir(parent)} value="..">
              <FolderIcon className="size-4" />
              <span>..</span>
            </CommandItem>
          )}
          {entries.map((entry) => {
            const full = dir === "" ? entry.name : `${dir}/${entry.name}`;
            return (
              <CommandItem
                key={full}
                onSelect={() => {
                  if (entry.is_dir) {
                    setDir(full);
                  } else {
                    actions.setSelectedFile(full);
                    onOpenChange(false);
                    setDir("");
                  }
                }}
                value={full}
              >
                {entry.is_dir ? <FolderIcon className="size-4" /> : <FileIcon className="size-4" />}
                <span className="truncate">{entry.name}</span>
              </CommandItem>
            );
          })}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  );
}
