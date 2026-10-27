// The one boundary between Bakery's Svelte state and the React agent panel.
//
// Everything crossing it is a plain, immutable value or a callback. No
// Svelte proxy, no store object, no reactive class instance is ever handed
// to React — React would not track it, and Svelte would not know React
// read it. The Svelte side recomputes a whole `AgentPanelSnapshot` whenever
// its reactive dependencies change and notifies subscribers; React consumes
// that through `useSyncExternalStore`. Nothing polls.
//
// This file is plain `.ts` on purpose: it is the shared vocabulary, so it
// must be importable from `.svelte.ts`, `.tsx` and the node-run `.check.ts`
// scripts alike.
import type {
  AccountCapabilities,
  AcpRuntimeSnapshot,
  AgentActivity,
  AttachmentRef,
  AttachmentUpload,
  ChildToolDecision,
  DirEntry,
  FileRead,
  ModelCatalog,
  OperatingMode,
} from "$lib/node";
import type { AskAnswer, AskQuestion, TranscriptItem } from "$lib/transcript";
import type { WaitingSwitch } from "./claudeProviders";
import type { QueuedItem } from "./panelState";

/** What the panel may render. Every field is a Bakery fact — if Bakery
 *  cannot answer it, it is absent here rather than invented in the UI. */
export type AgentPanelSnapshot = {
  streamId: string | null;
  agentId: string;
  /** Seat: "orchestrator" | "worker" | "baker". */
  roleId: string;
  /** Human label from the socket projection ("Orchestrator", "Baker", …). */
  roleLabel: string;
  /** The thread's name on screen ("Baker" for the Baker's main thread). */
  title: string;

  /** This agent's own transcript, already filtered by `eventsForAgent`. */
  items: readonly TranscriptItem[];
  busy: boolean;
  activity: AgentActivity | null;
  /** Last action error/notice from the conversation (send refusal, etc.). */
  notice: string;
  /** Counts event-socket reconnects; one can mean the node restarted, which
   *  ends every native session. */
  reconnects: number;

  /** No bound stream — the whole panel is inert. */
  disabled: boolean;
  /** A model is assigned and usable, so Send can do something. */
  canSend: boolean;
  /** Why not, when `canSend` is false — the projection's own reason. */
  unavailableReason: string | null;

  /** History is still loading, or failed to. Rendering an empty transcript
   *  as "nothing happened" while the fetch is in flight is a lie. */
  historyLoading: boolean;
  historyError: string;

  assignedModelId: string | null;
  /** A native Claude seat's provider: "subscription", "console" or "openrouter". */
  assignedProvider: string | null;
  /** What its model picker shows on the Bakery's own loop, and every turn
   *  runs on: the model and effort chosen, `""` for the runtime's own. */
  selection: { model: string; effort: string };
  /** A switch of model or provider asked for while this agent's turn runs:
   *  it lands when the turn ends, and the turn runs on meanwhile. */
  switching: WaitingSwitch | null;
  /** The seat's last switch when it couldn't land: which one, where it went,
   *  and why. */
  switchFailed: (WaitingSwitch & { id: string; error: string }) | null;
  /** The assigned account is a native runtime (ACP: Claude, Codex, Cursor,
   *  any configured plug). Its model, effort and context are settings of its
   *  live session, not per-turn overrides. */
  assignedNative: boolean;
  socketReady: boolean;
  /** Models assignable to THIS socket — already narrowed to this socket's
   *  own readiness by `deriveAvailableModels`. */
  availableModels: readonly { id: string; label: string }[];

  /** The socket's own worktree root, or "" when rootless (chat-only), and
   *  whether that path is actually present. Both are needed: a recorded root
   *  that has been deleted is not a browsable workspace, and offering a file
   *  picker over it would produce a fake tree. */
  worktreePath: string;
  worktreeOnDisk: boolean;
  selectedFile: string;
  /** Durable composer controls projected from Heart. They survive a pane
   * close/reopen because the panel owns no second queue or mode state. */
  operatingMode: OperatingMode;
  queuedInstructions: number;
  /** Immutable, causally unconsumed Heart queue entries in submission order. */
  queuedItems: readonly QueuedItem[];
  dispatchPaused: boolean;

  /** Undecided proposals on this socket, and the task baseline when this is
   *  a task socket. Deliberately two separate numbers: they describe
   *  different snapshots and must never be added together. */
  pendingProposals: number;
  taskBaseline: { added: number; removed: number } | null;

  /** The newest turn stopped short and can be resumed (seat-correct):
   *  "restart" — it died with the node; "failed" — it failed or hit its
   *  step limit. See `deriveInterrupted`. */
  interrupted: "restart" | "failed" | null;
  /** The newest unanswered `ask_user`, if any. */
  pendingAsk: { hash: string; questions: readonly AskQuestion[] } | null;
};

/** The result of one action.
 *
 *  `stale` is the third case on purpose. An action awaited across a socket
 *  rebind must not apply its answer to a panel that now shows a different
 *  identity, and it is equally wrong to show the human an error for
 *  something that did not fail — it simply no longer applies. Callers render
 *  nothing for `stale`. */
export type Outcome<T> = { status: "ok"; value: T } | { status: "error"; error: string } | { status: "stale" };

/** The node's acknowledgement for Stop. `paused` is durable queue state;
 * `cancelled` says whether there was an active operation for the runtime to
 * cancel. They are deliberately separate: Stop may successfully pause
 * dispatch when there is no cancellable turn. */
export type StopAcknowledgement = {
  cancelled: boolean;
  approval_cancelled?: boolean;
  paused: true;
  pause_event: string;
};

/** Everything the panel can *do*. Each one calls an action that already
 *  existed — this checkpoint adds no route and no second client. */
export type AgentPanelActions = {
  /** Resolves `error` when the node refused, so the composer keeps the
   *  human's text instead of clearing it. */
  send(text: string, opts: { filePath: string; steer?: boolean; attachmentIds?: string[] }): Promise<Outcome<void>>;
  cancel(): Promise<Outcome<StopAcknowledgement>>;
  resumeDispatch(): Promise<Outcome<void>>;
  setOperatingMode(mode: OperatingMode): Promise<Outcome<void>>;
  /** Moves a native-picker file into node-owned attachment storage before
   * its opaque id is committed to the durable instruction queue. */
  /** The file as picked, and for an image a model can't take whole its
   *  fitted copy too: every ref goes in the turn's attachment_ids. */
  uploadAttachment(file: AttachmentUpload): Promise<Outcome<AttachmentRef[]>>;
  /** Answers a pending ask — one answer per question, in order. */
  answer(hash: string, answers: AskAnswer[]): Promise<Outcome<void>>;
  /** Answers a native tool permission request (`agent.child_tool.pending`);
   *  the paused native call resumes with exactly this answer. */
  decideChildTool(hash: string, decision: ChildToolDecision): Promise<Outcome<void>>;
  /** Answers the Baker's ask for a folder (`agent.machine_access.requested`). */
  decideAccessRequest(hash: string, allow: boolean): Promise<Outcome<void>>;
  /** The operator's answer to a host the thread's sandbox tried, for its
   *  whole project (`*.domain` for any host under it); it resolves every
   *  ask it covers, in each thread that asked. */
  decideHost(projectId: string, host: string, allow: boolean): Promise<Outcome<void>>;
  /** The operator's answer to a secret-named file the thread's room was
   *  refused, for its whole project; a private key takes `sure`. */
  decideFile(projectId: string, path: string, allow: boolean, sure: boolean): Promise<Outcome<void>>;
  /** The operator's merge of a chef's waiting work into main; its
   *  `merge.done` closes the card. */
  mergeThread(streamId: string): Promise<Outcome<void>>;
  /** Accepts or rejects one pending proposal from the chat — the same node
   *  route Review calls, so the node's check that the file has not changed
   *  underneath applies, and one verdict event updates both surfaces. It
   *  never carries edited bytes: an edited acceptance needs the editor, so
   *  it stays in Review. */
  decideProposal(hash: string, accept: boolean): Promise<Outcome<void>>;
  /** Keeps (or undoes) every change one turn made on disk (turn snapshots),
   *  and says how many were decided and why any were refused. */
  reviewTurn(turnId: string, keep: boolean): Promise<Outcome<{ decided: number; refused: string[] }>>;
  /** One landed change's before and after, from Bakery's own copy: for a
   *  change the history keeps the content of out (operator only). */
  fetchChange(hash: string): Promise<Outcome<{ before: string | null; after: string | null }>>;
  /** The complete payload behind a bounded native tool event (`full.sha`). */
  fetchToolPayload(sha: string): Promise<Outcome<unknown>>;
  /** Folds every queued follow-up into ONE steer for the running turn. */
  steerAllQueued(): Promise<Outcome<void>>;
  /** Takes a waiting message back out of the queue (it will never send),
   *  with what it said, to edit (operator only). */
  withdrawQueued(id: string): Promise<Outcome<{ instruction: string; filePath: string | null; attachmentIds: string[] }>>;
  /** Always targets THIS panel's own seat — the server defaults to
   *  orchestrator, so a Baker or worker panel must pass its own (gap 1). */
  resume(): Promise<Outcome<void>>;
  /** A session that can't resume: retry it, or start fresh from the thread. */
  chooseSession(choice: "retry" | "fresh"): Promise<Outcome<void>>;
  /** A file a message was sent with, by its upload id. */
  fetchAttachment(id: string): Promise<Outcome<Blob>>;
  /** Persistent seat assignment. Does NOT create a new agent. Asked while
   *  the agent's turn runs, it waits for that turn to end (`switching`). */
  assignModel(modelId: string, provider?: string): Promise<Outcome<void>>;
  /** Sets the model and effort the picker shows and every turn runs on, until
   *  changed (`""` for the runtime's own), with how the picker names them. */
  select(choice: { model: string; effort: string; modelLabel?: string; effortLabel?: string }): Promise<Outcome<void>>;
  setSelectedFile(path: string): void;
  /** Opens (or focuses) the existing Review pane beside this one — on one
   *  change when its hash is given. */
  openReview(hash?: string): void;
  /** Lazily fetched, per the catalog contract: only when a picker opens. */
  fetchModelCatalog(modelId: string): Promise<Outcome<ModelCatalog>>;
  /** Agent/session-scoped account controls. Native runtimes initialize here
   * and return their exact generation; no React-owned capability cache. */
  fetchAccountCapabilities(modelId: string): Promise<Outcome<AccountCapabilities>>;
  /** Applies one advertised native option and resolves only after the
   * adapter returns the complete acknowledged replacement configuration. */
  setNativeConfig(generation: number, configId: string, value: string | boolean): Promise<Outcome<AcpRuntimeSnapshot>>;
  /** Stops this conversation's native runtime; its next use starts a fresh
   * one with the whole history. Refused while a turn runs. */
  restartRuntime(): Promise<Outcome<{ restarted: boolean }>>;
  /** Existing jailed directory listing, for the file picker. */
  listDir(path: string): Promise<Outcome<DirEntry[]>>;
  /** Reads a selected workspace file through the jail so a native runtime
   * can receive it as an embedded attachment rather than an edit target. */
  readFile(path: string): Promise<Outcome<FileRead>>;
};

export type AgentPanelStore = {
  /** React's `useSyncExternalStore` contract. */
  subscribe(onChange: () => void): () => void;
  /** Stable reference between changes — required by `useSyncExternalStore`. */
  getSnapshot(): AgentPanelSnapshot;
  actions: AgentPanelActions;
};
