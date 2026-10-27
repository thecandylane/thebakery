// Typed client for the bakery-node daemon.
//
// The webview is a thin projection: no Heart, no git, no model keys here. Every
// call below is a localhost request to the node, which owns all of that.

// `127.0.0.1:7717` is the one true default for every normal `pnpm dev` /
// Tauri / production run. `VITE_BAKERY_NODE_ADDR` exists only so a
// Playwright e2e harness can point a dynamically-ported Vite instance at its
// own dynamically-ported bakery-node instance instead of fighting a real
// running one over the fixed port (see `playwright.config.ts`). Optional
// chaining on `import.meta.env` itself (not just the property) is load-
// bearing, not stylistic: the `check:*` scripts (`transcript.check.ts` and
// anything importing it) run this file under plain `node`, outside Vite
// entirely, where `import.meta.env` doesn't exist at all — reading a
// property directly off `undefined` would throw before the `??` ever runs.
// `let`, not `const`: the desktop bootstrap reports the address the shell
// actually connected to, and ES module live bindings let every importer
// (events.ts, ProvidersPanel) follow that retarget without a refactor.
// Relative, with the extension: the plain-`node` check scripts load this file.
import { fitImageForModels } from "./attachmentInput.ts";
import { electronShell, isTauri } from "./desktop.ts";

export let NODE = import.meta.env?.VITE_BAKERY_NODE_ADDR || "127.0.0.1:7717";
export let HTTP = `http://${NODE}`;
export let WS = `ws://${NODE}/events`;

// ── Operator auth: the desktop bootstrap ─────────────────────────────────
// Bakery is a desktop application, and the operator credential reaches this
// window exactly one way: the shell (Electron on the Mac, Tauri on the
// phones; desktop.ts) reads it from ~/.bakery/node.json (0600, this user's
// own file), proves it against the running node, and returns it over IPC to
// its trusted window only. It then lives in memory for
// the session only — never localStorage, never a URL or query string,
// never a log line, never a frontend build variable, and never an agent's
// environment (agents are node-side processes with their own MCP-scoped
// credential).
//
// Outside the shell — only the Playwright harness — a token may be
// injected at RUNTIME on `window.__BAKERY_OPERATOR_TOKEN__`. That is a test
// seam, not a second onboarding path: nothing ships it and no build
// variable carries a credential.

let operatorTokenValue = "";

export function operatorToken(): string {
  if (operatorTokenValue) return operatorTokenValue;
  if (typeof window !== "undefined") {
    const injected = (window as unknown as { __BAKERY_OPERATOR_TOKEN__?: unknown }).__BAKERY_OPERATOR_TOKEN__;
    if (typeof injected === "string" && injected.trim()) return injected.trim();
  }
  return "";
}

/** True inside Bakery's own shell: Electron's bridge or Tauri's IPC hooks. */
export function isDesktopShell(): boolean {
  return electronShell() !== null || isTauri();
}

export type ConnectFailure = { kind: string; message: string };
export type ConnectResult = { ok: true; startedNode: boolean } | { ok: false; failure: ConnectFailure };

function asFailure(e: unknown): ConnectFailure {
  if (e && typeof e === "object" && "message" in e) {
    const kind = (e as { kind?: unknown }).kind;
    return {
      kind: typeof kind === "string" ? kind : "unknown",
      message: String((e as { message: unknown }).message),
    };
  }
  return { kind: "unknown", message: e instanceof Error ? e.message : String(e) };
}

/** The one startup step: ask the shell to connect us to the local node.
 *  Every failure comes back as one structured, actionable reason for the
 *  connection screen — never a raw 401 leaking out of some later fetch. */
export async function connectToNode(): Promise<ConnectResult> {
  if (!isDesktopShell()) {
    return operatorToken()
      ? { ok: true, startedNode: false }
      : {
          ok: false,
          failure: {
            kind: "not_desktop",
            message:
              "Bakery runs as a desktop application — open the Bakery app. (A browser tab has no way to reach the node's operator credential, by design.)",
          },
        };
  }
  try {
    let r: { addr: string; token: string; startedNode: boolean };
    const shell = electronShell();
    if (shell) {
      const c = await shell.connect();
      if (!c.ok) return { ok: false, failure: { kind: c.kind, message: c.message } };
      r = c;
    } else {
      const { invoke } = await import("@tauri-apps/api/core");
      const t = await invoke<{ addr: string; token: string; started_node: boolean }>("bakery_connect");
      r = { addr: t.addr, token: t.token, startedNode: t.started_node };
    }
    operatorTokenValue = r.token;
    if (r.addr && r.addr !== NODE) {
      NODE = r.addr;
      HTTP = `http://${NODE}`;
      WS = `ws://${NODE}/events`;
    }
    return { ok: true, startedNode: r.startedNode };
  } catch (e) {
    return { ok: false, failure: asFailure(e) };
  }
}

/** Spreadable auth header for every request to the node — `send()` applies
 *  it automatically; raw `fetch` callers (ProvidersPanel) spread it in. */
export function authHeaders(): Record<string, string> {
  const t = operatorToken();
  return t ? { authorization: `Bearer ${t}` } : {};
}

export type BakeryEvent = {
  stream_id: string;
  seq: number;
  content_hash: string;
  prev_hash: string | null;
  actor: string;
  ts: string;
  causality: string[];
  layer: string | null;
  sensitivity: string;
  family: string;
  payload: string;
  event_hash: string;
};

/** What a send answers: the queued event itself and, to an idle agent whose
 *  turn began at once, that turn's own `started` beside it. */
export type Queued = BakeryEvent & { started?: BakeryEvent | null };

/** The live work-trace state machine (07d-A5) — mirrors
 *  `bakery_node::state::ActivityPhase` field-for-field.
 *  `#[serde(tag = "phase", rename_all = "snake_case")]` on the Rust side
 *  gives this exact discriminated-union shape. */
export type ActivityPhase =
  | { phase: "contacting_model" }
  | { phase: "streaming"; text: string }
  | {
      phase: "running_tool";
      tool: string;
      call_id: string;
      started_at: string;
      kind: "tool" | "testing";
      preview: string | null;
      /** The newest line a running native command printed. */
      output?: string | null;
    }
  | { phase: "awaiting_approval"; kind: "file_edit" | "child_tool" | "question"; event_hash: string }
  | { phase: "complete"; note: string | null }
  | { phase: "error"; reason: string }
  | { phase: "cancelled" };

export type CompletedToolCall = { tool: string; call_id: string; ok: boolean; finished_at: string };

/** Always `null` until 07d-A3D1 Phase C populates real numbers — the field
 *  is wired through end to end now so that phase has nothing left to plumb. */
export type TurnUsage = {
  tokens_in: number | null;
  tokens_out: number | null;
  context_used: number | null;
  context_window: number | null;
};

/** `GET /agent/activity`'s response, and the payload every `"activity"`-kind
 *  WebSocket envelope carries verbatim (see `events.ts`'s `onActivity`) — a
 *  full current snapshot, never a delta, so a client that missed
 *  intermediate broadcasts is always caught up by the next one. */
export type AgentActivity = {
  agent_id: string;
  stream_id: string;
  turn_id: string;
  phase: ActivityPhase;
  /** When `phase` became this kind of phase: "Thinking… 4s" counts from it
   *  while the agent contacts its model. Absent from an older node. */
  phase_since?: string;
  turn_started_at: string;
  recent_completed_tools: CompletedToolCall[];
  usage: TurnUsage | null;
  /** The tokens the turn has produced so far, estimated from what it
   *  streamed (reply and thinking). Absent from an older node. */
  live_tokens?: number;
  cancel_requested: boolean;
};

export type WorktreeInfo = {
  path: string;
  branch: string | null;
  head: string | null;
  is_main: boolean;
  locked: boolean;
};

export type ProjectInfo = {
  path: string;
  is_git: boolean;
  error: string | null;
  root: string | null;
  name: string | null;
  current_branch: string | null;
  default_branch: string | null;
  local_branches: string[];
  worktrees: WorktreeInfo[];
};

/** A stream as recents needs it. */
export type StreamRef = {
  stream_id: string;
  name: string;
  /** The name the operator gave it (POST /conversation/rename); null keeps
   *  `name`, the one it was opened with. */
  title?: string | null;
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  exists_on_disk: boolean;
  /** True when `worktree_path` IS `repo_root` — opened in place, no linked worktree. */
  in_place: boolean;
  owner_node_id: string | null;
  last_ts: string;
  events: number;
  /** Latest `stream.lifetime.set` value; `"slice"` when never set (STEERING:
   *  slice is the default for "new session on this bug"). */
  lifetime: Lifetime;
  /** True once `stream.discarded` has landed — worktree/room are gone,
   *  Heart lineage is not. */
  discarded: boolean;
};

export type Lifetime = "living" | "slice" | "burn";

export type LifetimeResp = { stream_id: string; lifetime: Lifetime };

/** `POST /stream/discard` — what actually happened, not just "ok". Worktree
 *  removal and room destruction are attempted independently; either can fail
 *  without the other being skipped, and a partial failure still lands one of
 *  these (the request itself then rejects with a 5xx, but the outcome was
 *  recorded server-side either way — see `DiscardRecord`, node-side). */
export type DiscardResp = {
  stream_id: string;
  lifetime: Lifetime;
  worktree_removed: boolean;
  worktree_error?: string;
  room_destroyed: boolean;
  room_error?: string;
  /** A folder left in place, and why: it is, or holds, a project's root. */
  kept?: string;
  /** The chat's own folder went to the Trash. */
  chat_trashed?: boolean;
};

export type InspectResp = {
  project: ProjectInfo;
  suggested_name: string;
  /** Free branch name for `suggested_name` — lets the wizard preview before creating anything. */
  suggested_branch: string;
  /** This node's worktree base dir — the WORKTREE PATH half of the create preview. */
  worktree_base: string;
  bakery_streams: StreamRef[];
};

/** What is open — the context bar's content. */
export type ActiveContext = {
  stream_id: string;
  name: string;
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  exists_on_disk: boolean;
  /** True when `worktree_path` IS `repo_root` — opened in place, no linked worktree. */
  in_place: boolean;
  owner_node_id: string | null;
};

export type OpenResp = {
  event: BakeryEvent;
  context: ActiveContext;
  attached: boolean;
};

/** One row of `GET /gates` — the cross-stream "waiting on you" queue.
 *
 *  Carries NO file bodies by design: `added`/`removed` are precomputed on the
 *  node so the queue never ships `old_content`/`new_content` just to render
 *  `+3 / −1`. To see the bytes, open the stream (studio Review) — that is the
 *  surface that legitimately needs them.
 *
 *  `kind` is `"write"` for every row today. Elevated and handoff gates have no
 *  data source yet (NODE_API.md §8, §9) and the node never synthesizes them. */
export type Gate = {
  kind: "write";
  stream_id: string;
  /** The pending `agent.file.changed` hash — what `/agent/accept` wants. */
  event_hash: string;
  file_path: string;
  project: string;
  branch: string;
  /** Who proposed it (`agent:{model}`), not who decides it. */
  actor: string;
  ts: string;
  added: number;
  removed: number;
};

/** `GET /heart` — measured store stats, nothing modelled.
 *
 *  `chain` is absent unless the caller passed `?verify=1`: verification
 *  re-hashes every event, so the node never runs it implicitly and never
 *  claims a check it did not perform. `db_bytes` is null (not 0) when the
 *  Heart is in-memory — unknown, not empty. */
export type ChainCheck = {
  verified: boolean;
  streams_checked: number;
  /** Streams whose hash chain did not verify. Empty when `verified`. */
  broken: { stream_id: string; reason: string }[];
};

export type HeartStats = {
  streams: number;
  events: number;
  first_ts: string | null;
  last_ts: string | null;
  db_bytes: number | null;
  chain?: ChainCheck;
};

export type WriteMode = "manual" | "rules" | "auto";

export type NodeInfo = {
  node_id: string;
  hostname: string;
  stream_fallback: string;
  model: string;
  write_mode: string;
  worktree_base: string;
  max_tool_steps: number | null;
};

/** `GET /memory` — hot projection + durable L1 entries from `memory.written`. */
export type HotMemory = {
  lines: string[];
  paths: string[];
  prior_accept_path: string | null;
  scan_events: number;
  max_lines: number;
};

export type MemoryEntry = {
  id: string;
  layer: string;
  sensitivity: string;
  text: string;
  written_at: string;
  by: string;
  event_hash: string;
};

export type MemoryView = {
  stream_id: string;
  /** L1 durable notes (latest per id). Empty until POST /memory/write. */
  entries: MemoryEntry[];
  hot: HotMemory;
};

/** `GET /operator/snapshot` — measured factory rollup; no hostname. */
export type OperatorBakerJob = {
  branch: string;
  stream_id: string | null;
  brief: string | null;
  worktree_exists: boolean;
};

export type OperatorSnapshot = {
  write_mode: string;
  model: string;
  max_tool_steps: number | null;
  stream_count: number;
  pending_gates: number;
  events: number;
  baker: {
    known: boolean;
    job_count: number;
    base: string | null;
    has_origin: boolean | null;
    gh_available: boolean | null;
    jobs: OperatorBakerJob[];
    error: string | null;
  };
};

/** `GET /context` — measured prompt pieces + jail refusals.
 *
 *  Character counts, never tokens (the node has no tokenizer and records no
 *  usage). `refusals_total` is the uncapped count so a capped list can render
 *  "showing n of total" without looking complete when it is not. */
export type PromptChars = {
  system: number;
  memory: number;
  total: number;
};

export type ContextLimits = {
  max_tool_steps: number | null;
  hot_memory_scan_events: number;
  hot_memory_max_lines: number;
};

export type FileScope = {
  glob: string;
  mode: string;
};

export type Refusal = {
  at: string;
  tool: string;
  path: string | null;
  reason: string;
};

export type ContextView = {
  stream_id: string;
  prompt_chars: PromptChars;
  limits: ContextLimits;
  files_in_scope: FileScope[];
  refusals_total: number;
  refusals: Refusal[];
};

export type OpenArgs = {
  repo: string;
  name?: string;
  branch?: string;
  base_branch?: string;
  worktree_path?: string;
  /** 07d-A3: link the opened/attached socket to this Project in the same
   *  call — required, not best-effort; omit for a plain unlinked open. */
  project_id?: string;
};

/** One level of `/files/list` — directories first, already sorted, noise
 *  dirs (.git, node_modules, target, …) already filtered out by the node. */
export type DirEntry = { name: string; is_dir: boolean };

/** `secret`: the node found a credential file (an .env, a key…). */
export type FileRead = { path: string; text: string; digest: string; secret?: boolean };

/** `POST /files/write` — human save. `status: "conflict"` means the file on
 *  disk no longer matches the digest last read/saved (an agent accept, an
 *  external edit, …); `current_text` is `null` only when the file was
 *  deleted out from under the editor since. */
export type FileWrite =
  | { status: "ok"; path: string; digest: string }
  | { status: "conflict"; path: string; current_digest: string | null; current_text: string | null };

/** `POST /env/provision` — materialize (or cache-hit) this stream's
 *  *optional* pinned project toolchain (`mise.toml`). The room itself is
 *  always available (07b, thin room) — this route is only for the
 *  pinned-rustc capability, never admission. */
export type EnvProvisionResp = {
  digest: string;
  root: string;
  cache_hit: boolean;
  rustc: string | null;
};

/** `GET /env?stream_id=` — the room capability plane (07b/07b-B). `thin_ready`
 *  is true for any resolvable stream (thin admission has no prerequisites);
 *  `project_tools` is the *optional* pinned-toolchain capability, never a
 *  gate on chat/exec/terminal:
 *  - `"not_configured"` — no mise.toml at all. Not an error.
 *  - `"invalid"` — a mise.toml exists but doesn't parse.
 *  - `"unsupported"` — parses fine but pins something this backend can't provision.
 *  - `"configured"` — pins only supported tools but isn't cached yet (cold);
 *    exec/PTY will NOT auto-provision this, they attach thin instead.
 *  - `"ready"` — cached and warm; exec/PTY will actually use it.
 *  `reason` is present for `"invalid"` and `"unsupported"`. */
export type ProjectToolsStatus = "not_configured" | "invalid" | "unsupported" | "configured" | "ready";

export type EnvStatusResp = {
  thin_ready: boolean;
  project_tools: ProjectToolsStatus;
  reason?: string;
};

/** `POST /env/exec` (agent pipe, read-only workspace) or `POST /env/pty`
 *  (human pipe, read+write workspace) — the node picks the attach function
 *  by which route was called, never by a field in the body. */
export type EnvExecResp = {
  stdout: string;
  stderr: string;
  exit_code: number;
};

/** Node errors arrive as plain text bodies; surface them verbatim. Also the
 *  transport under the generated client (apiTransport.ts). */
export async function send<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${HTTP}${path}`, {
      ...init,
      headers: { ...authHeaders(), ...((init?.headers as Record<string, string>) ?? {}) },
    });
  } catch (e) {
    // fetch() rejects for network-level failures (node not listening, DNS,
    // CORS) with an opaque "TypeError: Load failed" / "Failed to fetch" that
    // names nothing a human can act on. Name the daemon and the fix instead.
    const reason = e instanceof Error ? e.message : String(e);
    throw new Error(`bakery-node not reachable at ${HTTP} — is the daemon running? (${reason})`);
  }
  const text = await res.text();
  if (!res.ok) {
    // Every 404 this node's own routes raise carries a real message (e.g.
    // "stream wt:x has no project.onboarded event") — an *empty* 404 body
    // only happens when Axum found no matching route at all, i.e. the
    // running node predates this client build. Name the fix, not just the
    // status code.
    if (res.status === 404 && text.trim().length === 0) {
      throw new Error(`bakery-node has no route for ${path} — restart bakery-node`);
    }
    throw new Error(text || `${res.status} ${res.statusText}`);
  }
  return JSON.parse(text) as T;
}

function post<T>(path: string, body: unknown): Promise<T> {
  return send<T>(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

/** A picker part after PromptInput has converted its Blob URL to a data URL.
 * Keep this intentionally structural so the node client does not import the
 * React-only `ai` package types. */
/** `sourcePath`: the thread's worktree file it was read from (an
 *  @-mention, the selected file), so the node can refuse one it keeps secret. */
export type AttachmentUpload = { url?: string; filename?: string; mediaType?: string; sourcePath?: string };
export type AttachmentRef = { id: string; filename: string; media_type: string; bytes: number };

async function dataUrlOf(blob: Blob): Promise<string> {
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => (typeof reader.result === "string" ? resolve(reader.result) : reject(new Error("attachment has no data URL")));
    reader.onerror = () => reject(new Error("could not read the selected attachment"));
    reader.readAsDataURL(blob);
  });
}

/** Uploads a picked file as it is, untouched. An image a model cannot take
 *  whole (over 2000 px or a few MB, or not PNG/JPEG) also goes fitted, as a
 *  second upload: the node sends the fitted copy with the message and gives
 *  the model the original's path to read. Both refs come back, original
 *  first, and both go in the turn's attachment_ids. */
async function uploadAttachment(stream_id: string, seat: string, file: AttachmentUpload): Promise<AttachmentRef[]> {
  if (!file.url) throw new Error("the selected attachment has no readable data");
  const filename = file.filename?.trim() || "attachment";
  let blob: Blob;
  let fitted: { blob: Blob; filename: string } | null;
  try {
    // A blob: or data: URL alike: the bytes.
    blob = await (await fetch(file.url)).blob();
    fitted = await fitImageForModels(blob, filename);
  } catch (e) {
    throw new Error(`could not read the selected attachment: ${e instanceof Error ? e.message : String(e)}`);
  }
  const upload = async (bytes: Blob, name: string, mediaType: string) =>
    post<AttachmentRef>("/agent/attachments", {
      stream_id,
      seat,
      filename: name,
      media_type: mediaType,
      data_url: await dataUrlOf(bytes),
      ...(file.sourcePath ? { source_path: file.sourcePath } : {}),
    });
  const refs = [await upload(blob, filename, file.mediaType?.trim() || "")];
  if (fitted) refs.push(await upload(fitted.blob, fitted.filename, fitted.blob.type));
  return refs;
}

export const node = {
  info: () => send<NodeInfo>("/node"),
  versions: () => send<VersionsReport>("/versions"),
  /** Asks the public npm registry for newer releases, then reports. */
  checkVersions: () => post<VersionsReport>("/versions/check", {}),
  /** The deployed build the app runs, source changes not in it yet, and the
   *  last Deploy / Restart / Roll back. */
  bakeryControl: () => send<BakeryControl>("/control/bakery"),
  /** The app reports its own errors into the node's log (`clientLog.ts`). */
  clientLog: (entry: { kind: string; message: string; detail: string; path: string }) =>
    post<Record<string, never>>("/client/log", entry),
  /** Runs `scripts/bakery.sh <action>` on the node. Every action stops the
   *  node part-way, so the app disconnects, then reconnects by itself. */
  runBakeryControl: (action: BakeryAction) => post<BakeryControl>("/control/bakery", { action }),
  machineAccess: () => send<MachineAccessView>("/machine-access"),
  agents: () => send<AgentsView>("/agents"),
  toolPolicy: () => send<KindPolicy[]>("/agents/tool-policy"),
  setToolPolicy: (kind: string, policy: ToolPolicyValue) =>
    post<KindPolicy[]>("/agents/tool-policy", { kind, policy }),
  agentStorage: () => send<StorageView>("/agents/storage"),
  storageList: (path: string) => send<StorageEntry[]>(`/agents/storage/list?path=${encodeURIComponent(path)}`),
  storageFile: (path: string) => send<StorageFile>(`/agents/storage/file?path=${encodeURIComponent(path)}`),
  storageDelete: (path: string) => post<{ deleted: string }>("/agents/storage/delete", { path }),
  /** Replaces the policy; idle Baker runtimes restart under it. */
  setMachineAccess: (policy: MachineAccessPolicy) => post<MachineAccessSaved>("/machine-access", policy),
  checkMachineAccess: (scope: MachineScope, project_id?: string) =>
    post<MachineCheck>("/machine-access/check", { scope, project_id: project_id ?? null }),
  /** One agent's reach; its runtime restarts under it, now or after its turn. */
  setAgentReach: (stream_id: string, reach: MachineReach) =>
    post<{ reach: MachineReach; restarted: boolean }>("/machine-access/reach", { stream_id, reach }),
  /** One agent's containers switch: Bakery's container tools, never a socket. */
  setAgentContainers: (stream_id: string, on: boolean) =>
    post<{ on: boolean; restarted: boolean }>("/machine-access/containers", { stream_id, on }),
  /** One container seat's CPUs and memory; null returns it to the default. */
  setSeatSize: (stream_id: string, size: SeatSize | null) =>
    post<{ size: SeatSize; restarted: boolean }>("/machine-access/seat-size", { stream_id, size }),
  /** Operator-only: sign Claude in for container seats, in a terminal pane. */
  claudeContainerSignIn: () => post<TerminalInfo>("/machine-access/claude-container/sign-in", {}),
  /** Whether the seat image is built from this node's recipe. */
  seatImage: () => send<{ image: string; state: "missing" | "stale" | "current" }>("/containers/seat-image"),
  /** The model proxy: installed, running, and where its upstream calls may go. */
  modelProxy: () => send<ModelProxyStatus>("/model-proxy"),
  /** Install the pinned model proxy release (sha256 checked) and start it. */
  installModelProxy: () => post<ModelProxyStatus>("/model-proxy/install", {}),
  /** Start the installed model proxy again, after fixing what stopped it. */
  startModelProxy: () => post<ModelProxyStatus>("/model-proxy/start", {}),
  /** Which providers the proxy holds keys for; never a key. */
  modelKeys: () => send<ModelKeys>("/model-proxy/keys"),
  /** Give the proxy an API key. The key goes to the proxy only. */
  addModelKey: (provider: ModelKeyProvider, key: string, label: string) =>
    post<ModelKeys>("/model-proxy/keys", { provider, key, label }),
  /** Take a key back from the proxy, by its fingerprint. */
  removeModelKey: (provider: ModelKeyProvider, id: string) => post<ModelKeys>("/model-proxy/keys/remove", { provider, id }),
  /** Build the seat image in a terminal pane the operator sees. */
  buildSeatImage: () => post<TerminalInfo>("/containers/seat-image/build", {}),
  /** One-time ticket for the `/events` WebSocket (upgrades cannot carry an
   *  Authorization header from a browser — see the node's operator_auth). */
  eventsTicket: () => post<{ ticket: string }>("/events/ticket", {}),
  /** Answer a pending `ask_user` question — resumes the parked turn with
   *  the answer as the ask call's own tool result (continue-after-decision). */
  answer: (stream_id: string, event_hash: string, answers: { picks: string[]; other?: string }[]) =>
    post<BakeryEvent>("/agent/answer", { stream_id, event_hash, answers }),
  /** Close a turn the node's restart killed and continue it as a fresh
   *  turn briefed from durable state (Checkpoint 2 item 3). */
  /** A sent file's bytes, as uploaded (GET /attachment, operator only). By
   *  hand: the generated `attachment` reads its answer as JSON, and this
   *  one is an image or any other file. */
  attachment: async (stream_id: string, id: string): Promise<Blob> => {
    const res = await fetch(`${HTTP}/attachment?stream_id=${encodeURIComponent(stream_id)}&id=${encodeURIComponent(id)}`, { headers: authHeaders() });
    if (!res.ok) throw new Error((await res.text()) || `${res.status} ${res.statusText}`);
    return res.blob();
  },
  resume: (stream_id: string, seat?: string) =>
    post<BakeryEvent>("/agent/resume", { stream_id, seat: seat ?? null }),
  inspect: (path: string) => post<InspectResp>("/project/inspect", { path }),
  open: (args: OpenArgs) => post<OpenResp>("/project/open", args),
  streams: () => send<StreamRef[]>("/streams"),
  /** Pending write proposals across every stream, oldest first (node-sorted). */
  gates: () => send<Gate[]>("/gates"),
  /** Pin/unpin (`living`), or flag `burn` — never automatic; always an
   *  explicit human (or Baker-on-the-operator's-behalf) action. */
  setLifetime: (stream_id: string, lifetime: Lifetime) =>
    post<LifetimeResp>("/stream/lifetime", { stream_id, lifetime }),
  /** Removes the worktree (only if Bakery created it — never the human's own
   *  in-place checkout) and destroys the room runtime; Heart lineage is
   *  never touched. `confirm` is required (and checked) only when the
   *  stream's current lifetime is `living`. */
  discardStream: (stream_id: string, confirm = false) =>
    post<DiscardResp>("/stream/discard", { stream_id, confirm }),
  /** Store stats. `verify` re-hashes every event — opt in, never on a poll. */
  heart: (verify = false) => send<HeartStats>(`/heart${verify ? "?verify=1" : ""}`),
  /** Full event history for one stream — the workpane's transcript on load. */
  history: (stream_id: string, init?: RequestInit) =>
    send<BakeryEvent[]>(`/stream/${encodeURIComponent(stream_id)}`, init),
  uploadAttachment: (stream_id: string, seat: string, file: AttachmentUpload) => uploadAttachment(stream_id, seat, file),
  turn: (stream_id: string, instruction: string, file_path?: string, seat?: string) =>
    post<BakeryEvent>("/agent/turn", {
      stream_id,
      instruction,
      file_path: file_path?.trim() ? file_path.trim() : null,
      seat: seat ?? null,
    }),
  /** Durable per-agent follow-up. This returns once Heart accepted the
   *  instruction; it does not wait for a model turn, so another instruction
   *  can be queued while one is running and pane closure cannot lose it. To
   *  an idle agent it also waits for the turn to begin (a moment), and says
   *  so in `started`. */
  queueTurn: (
    stream_id: string,
    instruction: string,
    file_path?: string,
    seat?: string,
    attachment_ids: string[] = [],
    steer = false,
  ) =>
    post<Queued>(steer ? "/agent/turn/steer" : "/agent/turn/queue", {
      stream_id,
      instruction,
      file_path: file_path?.trim() ? file_path.trim() : null,
      seat: seat ?? null,
      attachment_ids,
    }),
  /** Sets a conversation's selector: the model and effort every one of its
   *  turns runs on, until changed (`""` for the runtime's own). The labels
   *  are what its thread's "Model set to …" row shows. */
  select: (
    stream_id: string,
    seat: string,
    choice: { model: string; effort: string; modelLabel?: string; effortLabel?: string },
  ) =>
    post<Selection>("/agent/selection", {
      stream_id,
      seat,
      model: choice.model || null,
      model_label: choice.modelLabel ?? null,
      effort: choice.effort || null,
      effort_label: choice.effortLabel ?? null,
    }),
  /** The live work-trace bootstrap/reconnect snapshot (07d-A5) — `null` when
   *  no turn is (or has ever been) tracked for this agent. Call only when a
   *  Chat pane mounts or the event WebSocket reconnects, never on a poll
   *  timer — live updates arrive over `onActivity` instead. */
  activity: (stream_id: string, seat: string) =>
    send<AgentActivity | null>(
      `/agent/activity?stream_id=${encodeURIComponent(stream_id)}&seat=${encodeURIComponent(seat)}`,
    ),
  /** Stop pauses durable dispatch before cancelling the active turn. Queued
   *  instructions remain intact until the operator explicitly resumes. */
  cancelTurn: (stream_id: string, seat: string) =>
    post<{ cancelled: boolean; approval_cancelled?: boolean; paused: true; pause_event: string }>("/agent/turn/cancel", {
      stream_id,
      seat,
    }),
  resumeDispatch: (stream_id: string, seat: string) =>
    post<BakeryEvent>("/agent/dispatch/resume", { stream_id, seat }),
  /** Every follow-up waiting for this agent becomes ONE steer for its
   *  running turn (originals consumed exactly once). 409 when no turn runs
   *  or a queued item carries a file/attachment. */
  steerAllQueued: (stream_id: string, seat: string) =>
    post<BakeryEvent>("/agent/turn/queue/steer-all", { stream_id, seat }),
  /** Decide a native tool permission (`agent.child_tool.pending`). The
   *  paused native call resumes with exactly this answer. `allow_always`
   *  is node-wide for that tool KIND — callers must say so. */
  decideChildTool: (stream_id: string, event_hash: string, decision: ChildToolDecision) =>
    post<BakeryEvent>("/agent/pipe/gate/decide", { stream_id, event_hash, decision }),
  /** Answer the Baker's ask for a folder (`agent.machine_access.requested`).
   *  Allow adds a Baker grant (Settings → Machine access) and the
   *  conversation carries on with the folder reachable. */
  decideAccessRequest: (stream_id: string, event_hash: string, allow: boolean) =>
    post<BakeryEvent>("/machine-access/requests/decide", { stream_id, event_hash, allow }),
  /** Every agent and who started whom, with the subagents each native
   *  runtime launched in its latest turn — the Agent map pane. */
  agentMap: () => send<AgentMap>("/agents/map"),
  /** The complete payload behind an `agent.acp.tool_call` event whose Heart
   *  copy was bounded (`payload.full.sha`). */
  toolPayload: (stream_id: string, sha: string) =>
    send<unknown>(
      `/agent/tool-payload?stream_id=${encodeURIComponent(stream_id)}&sha=${encodeURIComponent(sha)}`,
    ),
  setOperatingMode: (stream_id: string, seat: string, mode: OperatingMode) =>
    post<BakeryEvent>("/agent/mode", { stream_id, seat, mode }),
  setNativeConfig: (
    stream_id: string,
    seat: string,
    generation: number,
    config_id: string,
    value: string | boolean,
  ) => post<AcpRuntimeSnapshot>("/agent/runtime/config", { stream_id, seat, generation, config_id, value }),
  /** `provider`: a native Claude seat's own setting ("subscription",
   *  "console" or "openrouter"); left out, the seat keeps the one it has. */
  seatAssign: (stream_id: string, seat: string, model_id: string, provider?: string) =>
    post<BakeryEvent>("/seat/assign", { stream_id, seat, model_id, ...(provider ? { provider } : {}) }),
  decide: (stream_id: string, event_hash: string, accept: boolean, new_content?: string) =>
    post<BakeryEvent>(`/agent/${accept ? "accept" : "reject"}`, {
      stream_id, event_hash, ...(accept && new_content !== undefined ? { new_content } : {}),
    }),
  /** `path` defaults to "." on the node; omit it for the worktree root. */
  filesList: (stream_id: string, path?: string) =>
    send<DirEntry[]>(
      `/files/list?stream_id=${encodeURIComponent(stream_id)}${path ? `&path=${encodeURIComponent(path)}` : ""}`,
    ),
  filesRead: (stream_id: string, path?: string) =>
    send<FileRead>(
      `/files/read?stream_id=${encodeURIComponent(stream_id)}${path ? `&path=${encodeURIComponent(path)}` : ""}`,
    ),
  /** Human save, jailed to the socket worktree. `expectedDigest` is the
   *  digest last read/saved for this file; `null` only for a file the
   *  editor never read (brand new). A stale digest comes back as
   *  `status: "conflict"` instead of overwriting the newer contents. */
  filesWrite: (stream_id: string, path: string, text: string, expectedDigest: string | null) =>
    post<FileWrite>("/files/write", { stream_id, path, text, expected_digest: expectedDigest }),
  /** Per-node, not per-stream — `stream_id` is cited only for the audit event. */
  setWriteMode: (write_mode: WriteMode, stream_id?: string) =>
    post<NodeInfo>("/policy/write-mode", { write_mode, stream_id: stream_id ?? null }),
  /** Hot projection + L1 entries for one stream. */
  memory: (stream_id: string) =>
    send<MemoryView>(`/memory?stream_id=${encodeURIComponent(stream_id)}`),
  /** Append durable L1 note (`memory.written`). Same id → replaces compiled truth. */
  memoryWrite: (args: {
    stream_id: string;
    text: string;
    id?: string;
    layer?: string;
    sensitivity?: string;
  }) => post<BakeryEvent>("/memory/write", args),
  /** One-shot measured factory rollup (no hostname). */
  operatorSnapshot: () => send<OperatorSnapshot>("/operator/snapshot"),
  /** Prompt char counts + jail refusals. Unknown stream → 400. */
  context: (stream_id: string) =>
    send<ContextView>(`/context?stream_id=${encodeURIComponent(stream_id)}`),
  /**
   * Baker factory assignment: linked worktree on `baker/<slug>` of The Bakery
   * repo. Never in-place on main checkout. Reuses the stream for the same brief slug.
   */
  bakerAssign: (brief: string) => post<BakerAssignResp>("/baker/assign", { brief }),
  /** baker/* branches + Heart streams on the bakery source root. */
  bakerWork: () => send<BakerWorkList>("/baker/work"),
  bakerWorkDetail: (sel: { branch?: string; stream_id?: string }) => {
    const q = new URLSearchParams();
    if (sel.branch) q.set("branch", sel.branch);
    if (sel.stream_id) q.set("stream_id", sel.stream_id);
    return send<BakerWorkDetail>(`/baker/work/detail?${q}`);
  },
  bakerPush: (branch: string) => post<BakerActionResp>("/baker/push", { branch }),
  bakerPr: (branch: string, title?: string, body?: string) =>
    post<BakerActionResp>("/baker/pr", { branch, title: title ?? null, body: body ?? null }),
  /** Requires human confirm:true — node refuses otherwise. Never force-push main. */
  bakerMerge: (branch: string, confirm: boolean) =>
    post<BakerActionResp>("/baker/merge", { branch, confirm }),
  /** Materialize this stream's optional pinned toolchain (cache hit on the
   *  second call). The room itself needs none of this — see `envStatus`. */
  envProvision: (stream_id: string) => post<EnvProvisionResp>("/env/provision", { stream_id }),
  /** The room capability plane: `thin_ready` (always true) plus the optional
   *  `project_tools` pinned-toolchain state. Never provisions anything itself. */
  envStatus: (stream_id: string) =>
    send<EnvStatusResp>(`/env?stream_id=${encodeURIComponent(stream_id)}`),
  /** Agent pipe: read-only workspace, writes confined to the room's own
   *  runtime dir. `argv` is split on whitespace client-side — no shell string. */
  envExec: (stream_id: string, argv: string[]) => post<EnvExecResp>("/env/exec", { stream_id, argv }),
  /** Human pipe (the studio Terminal): read+write workspace, via a real PTY. */
  envPty: (stream_id: string, argv: string[]) => post<EnvExecResp>("/env/pty", { stream_id, argv }),
  /** Node-owned named terminal sessions (07c-STUDIO-SHELL §3) — the process
   *  lives on the node, not in any one pane; closing a pane or reloading the
   *  client only ever detaches the view (see `terminalWsUrl`). */
  /** `command` is set only for a Codex/Cursor "Sign in" login pane
   *  (07d-A3D1) — `stream_id` must then be `SYSTEM_ACCOUNTS_STREAM_ID` and
   *  `mode` is ignored server-side (always an unsandboxed host process).
   *  `"agent"` opens the stream's read-only Agent terminal instead of
   *  spawning anything. */
  terminalCreate: (stream_id: string, mode: TerminalMode, name?: string, command?: LoginCommand) =>
    post<TerminalInfo>("/terminal/create", { stream_id, mode, name: name ?? null, command: command ?? null }),
  terminalList: (stream_id: string) =>
    send<TerminalInfo[]>(`/terminal/list?stream_id=${encodeURIComponent(stream_id)}`),
  terminalRename: (stream_id: string, terminal_id: string, name: string) =>
    post<TerminalInfo>("/terminal/rename", { stream_id, terminal_id, name }),
  /** Kills and reaps the whole process tree — distinct from merely detaching
   *  a view (closing the WebSocket, e.g. on pane close or reload) which
   *  leaves the session running for a later reattach. */
  terminalClose: (stream_id: string, terminal_id: string) => post<void>("/terminal/close", { stream_id, terminal_id }),
  /** Mints a one-time, short-lived ticket for `/terminal/ws` — WebSocket
   *  upgrades aren't CORS-governed, so any open tab could otherwise dial
   *  straight into a shell. This POST is; fetch a ticket right before
   *  opening the socket, never reuse one. */
  terminalTicket: (stream_id: string, terminal_id: string) =>
    post<TerminalTicketResp>("/terminal/ticket", { stream_id, terminal_id }),
  /** Attaches a view to an already-running terminal. `from_offset` (0 for a
   *  fresh attach) replays buffered output produced while nobody was
   *  watching before continuing live — the node keeps a bounded ring buffer
   *  per session regardless of whether any view is attached. `offsets`: the
   *  view counts bytes, and the node says where the replay starts and
   *  whatever is gone (terminalStream.ts). */
  terminalWsUrl: (stream_id: string, terminal_id: string, ticket: string, fromOffset = 0, offsets = false) =>
    `ws://${NODE}/terminal/ws?stream_id=${encodeURIComponent(stream_id)}&terminal_id=${encodeURIComponent(terminal_id)}&ticket=${encodeURIComponent(ticket)}&from_offset=${fromOffset}${offsets ? "&offsets=true" : ""}`,
  /** Whether `ghostty` is on the node's PATH — gates the optional "open in
   *  Ghostty" button; never a hard error when it's missing. */
  ghosttyAvailable: () => send<GhosttyAvailable>("/system/ghostty"),
  /** Opens this stream's worktree cwd in Ghostty on the node's host. No-op
   *  (quiet failure) if `ghostty` is not on PATH. */
  ghosttyOpen: (stream_id: string) => post<GhosttyOpenResp>("/system/ghostty/open", { stream_id }),
  studioState: () => send<StudioState>("/studio/state"),
  /** Bearer token for one openai-compat plug (`node.json`'s
   *  `openai_compat_plugs[].id`) — same discipline as `/auth/api-key`: the
   *  key is written straight to `secrets.json` and never read back, only
   *  the boolean is (07d-A3B). */
  plugSecretSet: (plug_id: string, key: string) =>
    post<PlugSecretStatus>("/auth/plug-secret", { plug_id, key }),
  plugSecretClear: (plug_id: string) =>
    post<PlugSecretStatus>("/auth/plug-secret/clear", { plug_id }),
  /** Provider plug CRUD (07d-A3B) — the supported way to add/edit/remove a
   *  named ACP or openai-compat plug definition; `id` is immutable once
   *  created (update changes everything else, never renames). */
  providersList: () => send<ProvidersListResp>("/providers"),
  acpPlugCreate: (
    id: string,
    argv: string[],
    label?: string,
    config?: AcpConfigCopy,
    env?: Record<string, string>,
  ) =>
    post<AcpPlugConfig>("/providers/acp-plug", {
      id,
      argv,
      label: label ?? null,
      config: config ?? null,
      env: env ?? {},
    }),
  acpPlugUpdate: (
    id: string,
    argv: string[],
    label?: string,
    config?: AcpConfigCopy,
    env?: Record<string, string>,
  ) =>
    post<AcpPlugConfig>("/providers/acp-plug/update", {
      id,
      argv,
      label: label ?? null,
      config: config ?? null,
      env: env ?? {},
    }),
  acpPlugDelete: (id: string) => post<void>("/providers/acp-plug/delete", { id }),
  openAiCompatPlugCreate: (id: string, base_url: string, model: string, label?: string) =>
    post<OpenAiCompatPlugConfig>("/providers/openai-compat-plug", { id, base_url, model, label: label ?? null }),
  openAiCompatPlugUpdate: (id: string, base_url: string, model: string, label?: string) =>
    post<OpenAiCompatPlugConfig>("/providers/openai-compat-plug/update", { id, base_url, model, label: label ?? null }),
  openAiCompatPlugDelete: (id: string) => post<void>("/providers/openai-compat-plug/delete", { id }),
  /** Creates a Project — no Git required (07d-A3). `root: undefined` is a
   *  chat-only Project (no root at creation time); Code/Files stay disabled
   *  on its starter socket until a root attaches. */
  /** A new loose chat: in no Project, with its own empty workspace inside
   *  Bakery (so native runtimes can run it); deleted with its room. */
  chatCreate: (title: string) => post<ActiveContext>("/chat/create", { title }),
  projectCreate: (name: string, root?: CreateProjectRootArgs, stacks?: ProjectStacks) =>
    post<CreateProjectResp>("/project/create", {
      name,
      root: root ?? null,
      stacks: stacks ?? defaultProjectStacks(),
    }),
  /** Preserves `project_id`; duplicate names are allowed (dedup is by
   *  canonical root path, never by name). */
  projectRename: (project_id: string, name: string) =>
    post<void>("/project/rename", { project_id, name }),
  projectPin: (project_id: string, pinned: boolean) =>
    post<void>("/project/pin", { project_id, pinned }),
  /** Relation-only: hides the Project, never deletes its roots/files/socket
   *  history. Active sockets fall back to Ungrouped on the next fetch. */
  projectRetire: (project_id: string) => post<void>("/project/retire", { project_id }),
  /** "Open any directory as a root" onto an existing Project — no git,
   *  no admission requirement beyond the path resolving to a directory. */
  projectRootOpen: (project_id: string, name: string, root: CreateProjectRootArgs) =>
    post<ActiveContext>("/project/root/open", { project_id, name, root }),
  /** Re-parents (or first-attaches) an already-open socket to a Project —
   *  the socket's own recorded root supplies the path; refused for a
   *  chat-only socket (nothing to attach). */
  projectSocketAttach: (stream_id: string, project_id: string) =>
    post<void>("/project/socket/attach", { stream_id, project_id }),
  projectSocketDetach: (stream_id: string) => post<void>("/project/socket/detach", { stream_id }),
  /** Codex/Cursor account status (07d-A3D1) — kept off `/auth/status` on
   *  purpose: each check shells out to the tool's own status subcommand, so
   *  a hung binary must never slow down every other provider row's poll.
   *  `force` bypasses the node's 10s per-tool cache. */
  cliAccountsStatus: (force = false) => send<CliAccountsStatus>(`/auth/cli-status${force ? "?force=1" : ""}`),
  /** One provider's live model/effort/context catalog (07d-A3D1 Phase B) —
   *  call only when a picker is actually opened, never on every render:
   *  the node caches this per provider, but the call itself is still a
   *  network round trip to this client. */
  modelCatalog: (id: string) => send<ModelCatalog>(`/models/catalog?id=${encodeURIComponent(id)}`),
  /** `refresh` asks the account's adapter now instead of answering from
   *  what it last reported — the model picker's Retry. */
  accountCapabilities: (id: string, stream_id?: string, seat?: string, refresh = false) => {
    const query = new URLSearchParams({ id });
    if (stream_id) query.set("stream_id", stream_id);
    if (seat) query.set("seat", seat);
    if (refresh) query.set("refresh", "true");
    return send<AccountCapabilities>(`/accounts/capabilities?${query}`);
  },
  claudeNativeStatus: () => send<ClaudeNativeStatus>("/auth/claude-native/status"),
  /** The permanent Baker's durable coordinates (07d-B Phase 1). Bootstraps
   *  on first hit; safe to call on every `/baker` page mount. */
  bakerHome: () => send<BakerHome>("/baker"),
  /** The Baker's parallel conversations (BAKER-THREADS-03). The sidebar reads
   *  them off `/studio/state` (`SocketInfo.baker_thread`); this route is the
   *  op registry's `thread_list` over HTTP. */
  bakerThreads: (includeArchived = false) =>
    send<BakerThreadView[]>(`/baker/threads${includeArchived ? "?include_archived=true" : ""}`),
  /** Opens a new thread under the one Baker. With an `instruction`, the new
   *  thread starts working on it immediately — in parallel. */
  bakerThreadCreate: (args: { title?: string; instruction?: string } = {}) =>
    post<CreateBakerThreadResp>("/baker/threads", {
      title: args.title ?? null,
      instruction: args.instruction ?? null,
    }),
  bakerThreadRename: (stream_id: string, title: string) =>
    post<BakerThreadView>("/baker/threads/rename", { stream_id, title }),
  /** Archive hides a thread (history kept, reversible); `false` restores it.
   *  Refused for the main conversation and for a thread with a running turn. */
  bakerThreadArchive: (stream_id: string, archived: boolean) =>
    post<BakerThreadView>("/baker/threads/archive", { stream_id, archived }),
  /** Archive (or restore) any conversation — a Baker thread or a project
   *  chat. Refused for the main Baker and while a turn is running. */
  archiveConversation: (stream_id: string, archived: boolean) =>
    post<{ archived: boolean }>("/conversation/archive", { stream_id, archived }),
  /** Stops a conversation's idle runtime; its next turn starts a fresh one
   *  with its history. Refused while a turn is running. */
  restartRuntime: (stream_id: string) => post<{ restarted: boolean }>("/conversation/restart", { stream_id }),
  /** Mints a task's durable identity only — idempotent per `(work_order,
   *  slug-of-title)` (07d-B Phase 3 item 5): a retry after a partial crash
   *  finds the existing task rather than minting a second one. */
  taskCreate: (args: { parent_project_id: string; title: string; work_order: string; contract?: TaskContractInput }) =>
    post<TaskCreateResp>("/task/create", { ...args, contract: args.contract ?? {} }),
  /** First act materializes the task's workspace (worktree + baseline for a
   *  git root, in-place binding for a plain-dir root, nothing for a
   *  chat-only task) and flips it to `running` — refused (stays `queued`)
   *  while any `depends_on` task is not yet `done`. Idempotent once running. */
  taskStart: (project_id: string, task_id: string) => post<TaskStartResp>("/task/start", { project_id, task_id }),
  /** Appends to the task's persisted inbox (07d-B Phase 3 item 4) — durable
   *  entry only; delivery/consumption is the Phase 4 driver's job, not
   *  guaranteed by this call alone. */
  taskMessage: (project_id: string, task_id: string, text: string) =>
    post<TaskMessageResp>("/task/message", { project_id, task_id, text }),
  taskCancel: (project_id: string, task_id: string, reason?: string) =>
    post<TaskCancelResp>("/task/cancel", { project_id, task_id, reason: reason ?? null }),
  /** Terminal — releases the task's worktree/room via the existing discard
   *  path. Refused if already retired. */
  taskRetire: (project_id: string, task_id: string) => post<TaskRetireResp>("/task/retire", { project_id, task_id }),
  /** Every `task.*` fact on one project's stream, folded into today's list,
   *  in creation order. */
  taskList: (project_id: string) => send<TaskView[]>(`/task/list?project_id=${encodeURIComponent(project_id)}`),
  /** The complete change set vs the task's materialized baseline — worktree
   *  vs `base_commit`, untracked files included, computed fresh on every
   *  call (07d-B Phase 4 — never cached; this is one of the three surfaces
   *  that always recomputes). Rejects for a task with no materialized git
   *  baseline yet (chat-only, or not yet started). */
  taskReview: (project_id: string, task_id: string) =>
    send<TaskReviewResp>(`/task/review?project_id=${encodeURIComponent(project_id)}&task_id=${encodeURIComponent(task_id)}`),
  /** Recomputes fresh, refuses if a pending proposal is still undecided,
   *  commits the reviewed state on the task's own branch (a no-op commit
   *  when the tree is already clean), and records `task.status.changed
   *  {done}` with that exact commit hash. A later change to the tree flips
   *  status back to `awaiting_review` at the next `taskReview`/driver wake —
   *  sign-off is never a one-way door that can silently go stale. */
  taskSignoff: (project_id: string, task_id: string, snapshot: string) =>
    post<TaskSignoffResp>("/task/signoff", { project_id, task_id, confirm: true, snapshot }),
  /** Task-scoped shipping (Checkpoint 1 item 3) — all human-confirmed,
   *  never automatic; the server refuses on any drift from the signed-off
   *  commit. Integrate pins the target branch the human approved. */
  taskPush: (project_id: string, task_id: string) => post<TaskShipResp>("/task/push", { project_id, task_id }),
  taskPr: (project_id: string, task_id: string) => post<TaskShipResp>("/task/pr", { project_id, task_id }),
  taskIntegrate: (project_id: string, task_id: string, target_branch: string) =>
    post<TaskShipResp>("/task/integrate", { project_id, task_id, confirm: true, target_branch }),
};

/** Reserved `stream_id` for terminals with no Project/socket context at all
 *  — today, a Codex/Cursor login pane spawned from Settings. Real stream ids
 *  are always `wt:`-prefixed. */
export const SYSTEM_ACCOUNTS_STREAM_ID = "sys:accounts";

/** Which CLI's interactive login `/terminal/create` should spawn — a closed
 *  set matching `bakery_agent::cli_tools::LoginCommand` exactly. */
export type LoginCommand = "codex-login" | "cursor-login" | "claude-native-login";

/** `installed`: the binary resolved and spawned at all. `linked`: its own
 *  status subcommand exited 0. */
export type CliAccountStatus = { installed: boolean; linked: boolean };
export type CliAccountsStatus = { codex: CliAccountStatus; cursor: CliAccountStatus };

export type ClaudeNativeStatus = {
  installed: boolean;
  connected: boolean;
  state: "subscription" | "signed_out" | "api_key" | "external" | "unknown" | "unavailable";
  label?: string;
  reason?: string;
  adapter_version: string;
  adapter_integrity: string;
  metadata_provenance: string;
};

/** `GET /versions` — what this node runs, read from what executes (never
 *  from a pinned constant); `latest` is only known after a check. */
export type VersionStatus = "ok" | "update_available" | "mismatch" | "missing";
export type VersionComponent = {
  id: string;
  label: string;
  installed: string | null;
  required: string | null;
  latest: string | null;
  status: VersionStatus;
  source: string;
  update: string;
};
export type VersionsReport = {
  build: {
    started_at: string | null;
    source_root: string | null;
    branch: string | null;
    commit: string | null;
    uncommitted_files: number | null;
    changed_since_start: string | null;
  };
  components: VersionComponent[];
  sessions: {
    stream_id: string;
    seat: string;
    account_id: string;
    runtime: string | null;
    version: string | null;
    model: string | null;
    outdated: boolean;
  }[];
  checked_at: string | null;
  check_error: string | null;
};

/** `GET /control/bakery` (D1) — the app runs from a deployed build, so agent
 *  edits change the source only until the operator deploys.
 *  `upgrade-adapter` (V1) installs the newest Claude adapter release the
 *  node's own update check found; the request never names a version. */
export type BakeryAction = "restart" | "deploy" | "rollback" | "upgrade-adapter";
export type DeployedBuild = {
  commit: string | null;
  built_at: string | null;
  /** Uncommitted files the build was made with. */
  dirty_files: number | null;
};
export type BakeryControl = {
  deployed: DeployedBuild | null;
  /** What Roll back would switch to. */
  previous: DeployedBuild | null;
  /** The node runs the deployed build, not one built from source. */
  running_deployed: boolean;
  /** Source files changed after the deployed build began (the first 20). */
  undeployed: string[];
  undeployed_count: number;
  last: {
    /** `deploy-ui` is the top bar's Hot reload, which Settings never starts. */
    action: BakeryAction | "deploy-ui";
    started_at: string;
    running: boolean;
    /** `null` while running, or when the script ended without a verdict. */
    ok: boolean | null;
    /** Why it failed: 3 from `upgrade-adapter` means the Bakery patch
     *  needs porting to the new release. */
    exit_code: number | null;
    log_tail: string[];
  } | null;
};

/** One node of the Agent map (`GET /agents/map`). */
export type AgentMapNode = {
  id: string;
  kind: "baker" | "thread" | "task" | "chat" | "project" | "subagent";
  label: string;
  /** The model it runs on, or a subagent's type. */
  detail: string | null;
  status: "working" | "needs_input" | "failed" | "done" | "idle" | "stopped";
  note: string | null;
  /** The node that started it; null for a top-level agent. */
  parent: string | null;
  stream_id: string;
  seat: string;
  /** A subagent's launching tool call. */
  tool_call_id: string | null;
  /** The tool calls a subagent made so far. */
  steps: number;
  last_ts: string;
};
export type AgentMap = { nodes: AgentMapNode[] };

/** Settings → Machine access: what the Baker's native runtime may read,
 *  write, or run beyond its sandboxed workspace. */
export type MachineGrantAccess = "read" | "write" | "exec" | "full";
/** Who a grant reaches: the Baker, the other agents (project agents and task
 *  workers), or both. */
export type MachineScope = "baker" | "agents" | "all_agents";
export type MachineGrant = {
  path: string;
  access: MachineGrantAccess;
  scope: MachineScope;
  on_path: boolean;
  preset: string | null;
};
export type MachineEnvVar = { name: string; value: string; scope: MachineScope; preset: string | null };
export type MachineAccess = { grants: MachineGrant[]; env: MachineEnvVar[] };
/** One agent's place on the scope dial: its room (workspace plus grants) or
 *  the whole Mac minus the deny list. */
export type MachineReach = "host_full" | "host_room" | "container" | "locked";
/** Global grants (each for the Baker, every project, or both) plus each
 *  project's own environment, keyed by project id; the operator's additions
 *  to the deny list; each conversation's reach (set per agent, never saved
 *  with the policy). */
export type MachineAccessPolicy = MachineAccess & {
  projects: Record<string, MachineAccess>;
  deny: string[];
  reach: Record<string, MachineReach>;
  /** Hosts container seats may also reach; `.example.com` for subdomains. */
  allow_hosts: string[];
};
export type MachineAccessPreset = {
  id: string;
  label: string;
  detail: string;
  grants: MachineGrant[];
  env: MachineEnvVar[];
  unavailable: string | null;
};
export type MachineAccessView = {
  policy: MachineAccessPolicy;
  presets: MachineAccessPreset[];
  always_denied: string[];
  /** The hosts every container seat may reach. */
  seat_hosts: string[];
  live: { stream_id: string; busy: boolean; baker: boolean }[];
  /** Granted folders that are gone: kept, granted to no one until back. */
  missing: string[];
  /** Granted folders that now resolve to what is never granted, with why. */
  blocked: Record<string, string>;
  /** The operator's home, shown as `~`. */
  home: string;
};
/** `POST /machine-access/check`: run inside a real agent sandbox. */
export type MachineCheck = {
  tools: { name: string; works: boolean; path: string; detail: string }[];
  guards: { what: string; result: string; held: boolean }[];
  path: string;
};
export type MachineAccessSaved = MachineAccessView & { restarted: string[]; busy: string[] };

/** Settings → Agents: every agent on every socket, whatever backs it. */
export type ToolPolicyValue = "always" | "ask" | "never";
export type KindPolicy = { kind: string; policy: ToolPolicyValue; read_only: boolean };
export type AgentTool = { name: string; summary: string; read_only: boolean };
export type AgentSandbox = {
  home: string;
  writes: string[];
  reads: string[];
  runs: string[];
  never_writes: string[];
};
export type AgentAbility = {
  stream_id: string;
  seat: string;
  agent_id: string;
  socket: string;
  scope: "baker" | "task" | "project";
  model_id: string | null;
  transport: "native" | "api" | "none";
  busy: boolean;
  runtime: AcpRuntimeSnapshot | null;
  bakery_tools: AgentTool[];
  sandbox: AgentSandbox | null;
  reach: MachineReach;
  /** May use containers, through Bakery's container tools only. */
  containers: boolean;
  /** Its container seat's CPUs and memory when set; null is the default. */
  seat_size: SeatSize | null;
  /** Its calls through the model proxy; null before its first. */
  model_usage: AgentUsage | null;
};
/** One agent's calls through the model proxy, all time. */
export type AgentUsage = {
  calls: number;
  /** Refused, rate-limited, or the provider erred. */
  failed: number;
  tokens_in: number;
  tokens_out: number;
  /** Calls a model other than the one asked for answered. */
  pin_mismatches: number;
  /** When its last call went through (RFC 3339). */
  last_at: string;
};
export type AgentsView = {
  agents: AgentAbility[];
  tool_policy: KindPolicy[];
  write_mode: string;
  /** What a container seat gets of this Mac unless set per agent. */
  seat_default: SeatSize;
};
/** A container seat's share of this Mac. */
export type SeatSize = { cpus: number; memory_gib: number };
/** A provider whose API keys the model proxy holds. */
export type ModelKeyProvider = "claude" | "gemini" | "xai";
/** The keys the proxy holds, by provider: labels and dates, never a key. */
export type ModelKeys = {
  providers: { provider: ModelKeyProvider; keys: { id: string; label: string; added: string }[] }[];
};
/** The model proxy (CLIProxyAPI), as the node runs it. */
export type ModelProxyStatus = {
  installed: boolean;
  version: string;
  running: boolean;
  healthy: boolean;
  port: number;
  error: string | null;
  egress_hosts: string[];
};

/** Settings → Agent storage: every place an agent writes outside its project. */
export type StorageArea = {
  label: string;
  path: string;
  clearable: boolean;
  exists: boolean;
  bytes: number;
  files: number;
  newest: string | null;
  truncated: boolean;
};
export type AgentStorage = {
  stream_id: string;
  seat: string;
  socket: string;
  scope: "baker" | "task" | "project";
  model_id: string | null;
  running: boolean;
  areas: StorageArea[];
  project_files: string[];
};
export type StorageView = {
  agents: AgentStorage[];
  shared: StorageArea[];
  host_side: { what: string; detail: string }[];
  never_in_projects: string[];
};
export type StorageEntry = { name: string; dir: boolean; bytes: number; modified: string | null; credential: boolean };
export type StorageFile = { text: string; bytes: number; truncated: boolean; binary: boolean };

export type PtyMode = "host" | "room";
/** A pty's mode, `"agent"`: the stream's read-only record of every command
 *  its agents run, written by the node — no process behind it — or `"job"`:
 *  one long command an agent started (`job_start`). */
export type TerminalMode = PtyMode | "agent" | "job";

/** One node-owned named terminal session — `live: false` means the process
 *  has exited; the row still existed a moment ago but is no longer attachable. */
export type TerminalInfo = {
  terminal_id: string;
  stream_id: string;
  name: string;
  mode: TerminalMode;
  cwd: string;
  live: boolean;
  created_ts: string;
};

export type TerminalTicketResp = { ticket: string; expires_in_secs: number };

export type GhosttyAvailable = { available: boolean };
export type GhosttyOpenResp = { opened: boolean };

/** `GET /baker` — the permanent Baker's durable coordinates (07d-B Phase 1),
 *  mirroring `bakery_node::routes::baker::BakerHome` field-for-field.
 *  Bootstraps the permanent socket on first hit; idempotent ever after. Its
 *  `agent_id` is `${stream_id}:baker` — stable across restarts and model
 *  reassignment, so a Chat pane bound to it never needs to reconstruct the
 *  id client-side (same rule `agentIdFor` in `DockviewHost.svelte` follows
 *  for ordinary sockets). */
export type BakerHome = {
  stream_id: string;
  agent_id: string;
  /** The read-scoped `bakery_source_root`, or `""` when unknown — the
   *  identity exists either way. */
  root: string;
  assigned_model_id: string | null;
  /** Names of every declared Bakery op this stream's turns can call. */
  ops: string[];
};

/** `task.status.changed`'s closed status vocabulary (07d-B Phase 3) —
 *  mirrors `bakery_node::tasks::TaskStatus`'s `#[serde(rename_all =
 *  "snake_case")]` variants exactly. */
export type TaskStatus = "queued" | "running" | "blocked" | "awaiting_review" | "done" | "cancelled";

/** A task's completion contract — mirrors `bakery_node::tasks::TaskContract`.
 *  `owned_files`/`proof`/`depends_on` are advisory/declared, never enforced
 *  by this client (07d-B Phase 3 item 6: ownership stays advisory). */
export type TaskContract = {
  scope: string;
  owned_files: string[];
  depends_on: string[];
  /** argv lists a human/Baker runs before signing this task off. */
  proof: string[][];
  turn_budget: number | null;
};

/** Every field of [`TaskContract`] is optional on the wire (server-side
 *  `#[serde(default)]`) — what `taskCreate` actually needs to send. */
export type TaskContractInput = Partial<TaskContract>;

/** One task, folded from its parent project's `task.*` facts — mirrors
 *  `bakery_node::tasks::TaskView`. `retired` is the only reliable terminal
 *  signal (07d-B: `status` alone never goes to a "retired" value). */
export type TaskView = {
  task_id: string;
  title: string;
  slug: string;
  parent_project_id: string;
  work_order: string;
  contract: TaskContract;
  /** The task's own lightweight socket — chat-capable immediately, even
   *  before `taskStart` materializes a workspace. */
  stream_id: string;
  status: TaskStatus;
  status_reason: string | null;
  /** `owned_files` overlaps with other non-dependent tasks, advisory only. */
  warnings: string[];
  retired: boolean;
};

export type TaskCreateResp = { task: TaskView; created: boolean };
export type TaskStartResp = {
  task: TaskView;
  materialized: boolean;
  worktree_path: string | null;
  branch: string | null;
  base_commit: string | null;
};
export type TaskMessageResp = { event: BakeryEvent };
export type TaskCancelResp = { task: TaskView; turn_cancel_requested: boolean };
export type TaskRetireResp = { task: TaskView; discard: DiscardResp };

/** `GET /task/review`'s response shape (07d-B Phase 4: complete baseline
 *  diff — worktree vs `task.workspace.materialized`, not just the recorded
 *  proposal log — plus sign-off/commit-pinning state). Mirrors
 *  `bakery_node::task_review::{TaskReviewResp, ReviewFile}` field-for-field;
 *  `status` is the one deliberate addition over this type's earlier
 *  placeholder shape (name-status per the spec, absent from the pre-Stage-D
 *  stub). Refuses (server-side, `GET /task/review` 400s) for a task with no
 *  materialized git baseline — chat-only, or not yet started. */
export type TaskReviewResp = {
  task_id: string;
  base_commit: string | null;
  files: {
    path: string;
    /** `"unreviewable"` is a git status letter this surface cannot render as
     *  an old/new diff (today, only `U` — unmerged); still a visible row,
     *  never silently dropped. */
    status: "added" | "modified" | "deleted" | "unreviewable";
    added: number;
    removed: number;
    old_content: string;
    new_content: string;
    /** Why displayed content is not the complete bytes, when it isn't —
     *  "binary" | "truncated for display" | "refused" | "oversize". */
    note?: string;
    /** false when the bytes could not even be hashed (refused/oversize/
     *  unreadable) — sign-off refuses while any such file exists. */
    reviewable: boolean;
  }[];
  signed_off: boolean;
  signoff_commit: string | null;
  /** Content-derived id of exactly this reviewed state — must be sent back
   *  on sign-off; the server refuses when the workspace no longer matches. */
  snapshot: string;
  /** The project repo's currently checked-out branch — what integrate
   *  would merge into; sent back as the pinned target and re-verified
   *  server-side at merge time. */
  repo_current_branch?: string;
};

export type TaskSignoffResp = { task: TaskView; signoff_commit: string };

/** One task-scoped ship op's result (`/task/push`, `/task/pr`, `/task/integrate`). */
export type TaskShipResp = { task_id: string; branch: string; detail: string };

/** `POST /baker/assign` — dedicated Baker session on The Bakery source tree. */
export type BakerAssignResp = {
  event: BakeryEvent;
  context: ActiveContext;
  attached: boolean;
  /** Node write gate; default Manual for first dogfood (propose → accept). */
  write_mode: WriteMode;
  brief: string;
  bakery_source_root: string;
};

/** One baker/* row from GET /baker/work. */
export type BakerWorkItem = {
  branch: string;
  stream_id: string | null;
  worktree_path: string | null;
  worktree_exists: boolean;
  brief: string | null;
  /** True when this branch's most recent known stream has been discarded —
   *  distinct from `stream_id === null` alone, which is also true for a
   *  branch that never had any stream. Server-computed (07c-LIVE-STATE) so
   *  the client never needs to compare repo_root strings itself — a browser
   *  has no filesystem access to canonicalize them safely. */
  discarded: boolean;
};

export type BakerWorkList = {
  bakery_source_root: string;
  base: string | null;
  items: BakerWorkItem[];
  git_error: string | null;
  has_origin: boolean;
  gh_available: boolean;
};

export type BakerCommit = { sha: string; subject: string };
export type BakerStatusEntry = { code: string; path: string };

export type BakerWorkDetail = {
  branch: string;
  base: string | null;
  stream_id: string | null;
  worktree_path: string | null;
  worktree_exists: boolean;
  brief: string | null;
  commits: BakerCommit[];
  commits_error: string | null;
  diff_stat: string | null;
  diff_error: string | null;
  status: BakerStatusEntry[];
  status_error: string | null;
  has_origin: boolean;
  gh_available: boolean;
  bakery_source_root: string;
};

export type BakerActionResp = {
  ok: boolean;
  message: string;
  branch: string;
};

/** One explicitly named directory copied into an ACP seat's isolated HOME
 *  (07d-A3B) — mirrors `crates/bakery-node/src/node.rs`'s `AcpConfigCopy`. */
export type AcpConfigCopy = { source: string; home_relative_destination: string };

/** A named ACP CLI plug definition (07d-A3B) — `node.json`'s
 *  `acp_plugs[]`. `argv[0]` is the binary (bare name resolves via PATH). */
export type AcpPlugConfig = {
  id: string;
  label?: string | null;
  argv: string[];
  config?: AcpConfigCopy | null;
  env?: Record<string, string>;
};

/** A named openai-compat local-model plug definition (07d-A3B) —
 *  `node.json`'s `openai_compat_plugs[]`. Bearer (if any) lives in
 *  `secrets.json`, never here — see `plugSecretSet`/`plugSecretClear`. */
export type OpenAiCompatPlugConfig = {
  id: string;
  label?: string | null;
  base_url: string;
  model: string;
};

export type ProvidersListResp = {
  acp_plugs: AcpPlugConfig[];
  openai_compat_plugs: OpenAiCompatPlugConfig[];
};

export type ModelOption = {
  id: string;
  label: string;
  transport: "http" | "acp" | "openai-compat";
  node_available: boolean;
  reason?: string;
};

/** One selectable model within a provider's live catalog (07d-A3D1 Phase B)
 *  — `id`/`label` come straight from the provider's own list-models
 *  response, never a hardcoded table. */
export type ModelCatalogEntry = {
  id: string;
  label: string;
  context_window?: number | null;
  effort_levels: EffortLevel[];
};

export type EffortLevel = { id: string; label: string };

/** `GET /models/catalog?id=<provider_id>`'s response — `Available` for a
 *  provider with a real list-models endpoint and configured credentials,
 *  `Unavailable` otherwise (no credentials, a provider with nothing to
 *  enumerate like Claude Code, or a transient fetch error — `reason` says
 *  which). Node-cached with a background refresh (`crate::model_catalog`);
 *  calling this repeatedly is cheap. */
export type ModelCatalog =
  | { status: "available"; models: ModelCatalogEntry[] }
  | { status: "unavailable"; default: ModelCatalogEntry; reason: string };

/** The account-adapter boundary behind the composer picker. `source` tells
 * whether values came from a live provider, a subscription adapter report,
 * or a configured endpoint; absent capabilities are never guessed. */
export type AccountCapabilities = {
  source:
    | "live_provider"
    | "subscription_adapter"
    | "native_session"
    | "remembered"
    | "configured_endpoint"
    | "unavailable";
  catalog: ModelCatalog;
  /** `false` for an account with no model list at all (nothing to show). */
  catalog_expected?: boolean;
  text_attachments: boolean;
  image_attachments: boolean;
  audio_attachments: boolean;
  plan_enforceable: boolean;
  native_runtime?: AcpRuntimeSnapshot;
  availability_reason?: string;
  metadata_provenance: string;
};

export type AcpRuntimeIdentity = { name: string; title?: string; version: string };
export type AcpAuthMethod = {
  id: string;
  name: string;
  description?: string;
  kind: "agent" | "terminal" | "environment" | "unknown";
};
export type AcpAuthStatus = { kind: string; label?: string; detail?: string; plan?: string };
export type AcpConfigValueOption = {
  id: string;
  name: string;
  description?: string;
  group?: string;
  group_name?: string;
};
export type AcpConfigOption = {
  id: string;
  name: string;
  description?: string;
  category?: string;
  kind: "select" | "boolean" | "unsupported";
  current_value: string | boolean | null;
  values: AcpConfigValueOption[];
};
export type AcpMode = { id: string; name: string; description?: string };
export type AcpCommand = { name: string; description: string; input_hint?: string };
export type AcpRuntimeSnapshot = {
  account_id: string;
  generation: number;
  runtime?: AcpRuntimeIdentity;
  auth_methods: AcpAuthMethod[];
  auth_status?: AcpAuthStatus;
  config_options: AcpConfigOption[];
  modes: AcpMode[];
  current_mode?: string;
  commands: AcpCommand[];
  prompt: { image: boolean; audio: boolean; embedded_context: boolean };
  steering: boolean;
  native_review: boolean;
  requested_model?: string;
  effective_model?: string;
  usage?: { used: number; size: number };
  provenance: string;
};

/** The per-agent composer capability boundary, deliberately distinct from
 * the node-wide WriteMode. Mirrors `agent_control::OperatingMode`. */
export type OperatingMode = "manual" | "plan" | "auto_edit" | "auto";

export type RoleOption = { id: string; label: string };

/** Response from `/auth/plug-secret` and `/auth/plug-secret/clear` — the
 *  boolean is the whole vocabulary; the key itself is never echoed back. */
export type PlugSecretStatus = { plug_id: string; secret_set: boolean };

/** A conversation's selector: its model and effort, `null` for the runtime's own. */
export type Selection = { model?: string | null; effort?: string | null };

export type AgentInfo = {
  agent_id: string;
  role_id: string;
  role_label: string;
  assigned_model_id: string | null;
  /** A native Claude seat's provider: "subscription", "console" or "openrouter". */
  assigned_provider?: string | null;
  /** A switch asked for while this seat's turn runs: it lands when that turn
   *  ends. Absent when none waits (and from an older node). */
  switching?: { model_id: string; provider?: string | null } | null;
  /** The seat's last switch when it couldn't land and nothing has moved the
   *  seat since: which one (`id`), where it went, and why. */
  switch_failed?: { id: string; to: { model_id: string; provider?: string | null }; error: string } | null;
  socket_ready: boolean;
  /** What its picker shows on the Bakery's own loop, and every turn runs on:
   *  the model and effort chosen, `null` for the runtime's own. */
  selection?: Selection;
  /** A turn is currently in flight for this agent (07d-A) — one busy/run
   *  state per agent_id, not per socket. */
  busy: boolean;
  reason?: string;
};

/** One model's usability on THIS socket specifically — distinct from
 *  `ModelOption.node_available`, which is node-wide. The picker's source of
 *  truth: an HTTP model's readiness never varies by socket, but ACP's does
 *  (e.g. its workspace missing from disk), so `ModelOption` alone cannot
 *  drive the <select>. */
export type SocketModelOption = {
  id: string;
  ready: boolean;
  reason?: string;
};

/** `"task"` is emitted for every `SocketKind::Task` socket as of 07d-B
 *  Phase 3 (a task's own lightweight socket, bucketed under its parent
 *  Project) — no longer reserved/unreachable. `discarded` still wins over
 *  it: a retired task's socket reports `"retired"`, not `"task"`, the same
 *  precedence any other discarded socket gets.
 *
 *  07c-LIVE-STATE: `/studio/state` is the client's only socket inventory —
 *  this carries every `StreamRef` field one-to-one (do not go back to a
 *  shadow `/streams` fetch for anything this type is missing; add the field
 *  here and on the server's `SocketInfo` instead).
 *
 *  07d-A3: `project` is no longer this socket's own recorded git-repo name
 *  verbatim — for a socket grouped under a Project, it is that Project's
 *  CURRENT (possibly renamed) display name, resolved fresh on every
 *  request; only an Ungrouped or factory socket falls back to its own
 *  recorded name. `project_id`/`root_id` are `null` for both of those. */
export type SocketInfo = {
  stream_id: string;
  name: string;
  /** The name the operator gave it (POST /conversation/rename); null keeps
   *  `name`, the one it was opened with. */
  title?: string | null;
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  exists_on_disk: boolean;
  in_place: boolean;
  owner_node_id: string | null;
  last_ts: string;
  events: number;
  lifetime: Lifetime;
  /** Raw `stream.discarded` truth — `state` below is the same fact spelled
   *  as a friendlier string. */
  discarded: boolean;
  state: "active" | "retired" | "task";
  agents: AgentInfo[];
  available_models: SocketModelOption[];
  project_id: string | null;
  root_id: string | null;
  /** Undecided write proposals on this exact socket (07d-B Phase 4 item 3) —
   *  `routes::operator::gates_inner` filtered to `stream_id`. */
  pending_gates: number;
  /** Lines added/removed in the COMPLETE baseline diff (worktree vs
   *  `task.workspace.materialized`, not the recorded-proposal log) for a
   *  materialized task socket — `0`/`0` for every non-task socket, and for a
   *  task with no materialized baseline yet. Bounded-staleness cached
   *  server-side; the Review pane's own `taskReview`/`taskSignoff` calls
   *  never read these, they recompute fresh. */
  changed_added: number;
  changed_removed: number;
  /** Set on the permanent Baker's own socket (`main: true`) and on each of
   *  its parallel threads (docs/slices/BAKER-THREADS-03.md); `null` on every
   *  other socket. Absent only when talking to an older node. */
  baker_thread?: BakerThreadInfo | null;
  /** Hidden from conversation lists, history kept (`POST /conversation/archive`).
   *  Absent only when talking to an older node. */
  archived?: boolean;
  /** A chat with a folder of its own: Delete moves its files to the Trash,
   *  and it can be made a project. Absent only when talking to an older node. */
  chat?: boolean;
  /** Where the conversation stands — the node's one answer for every list.
   *  Absent only when talking to an older node. */
  status?: ConversationStatus;
  /** One line on why: what it waits on, or how its last turn ended. */
  status_note?: string | null;
  /** The conversation that opened this one as its peer: a worker's orc.
   *  Absent only when talking to an older node. */
  opened_by?: string | null;
  /** A workspace's orc: a conversation beneath the Baker on a worktree of
   *  its own, whose workers name it in `opened_by`. */
  workspace?: boolean;
};

/** `working` — a turn is running; `needs_input` — a change to review, a
 *  permission prompt, a question, or a reply that asked you something;
 *  `failed`; `done` — finished, nothing waiting (close it when finished);
 *  `idle` — nothing run yet, or stopped. */
export type ConversationStatus = "working" | "needs_input" | "failed" | "done" | "idle";

/** `POST /agent/pipe/gate/decide` answer for a native tool permission. */
export type ChildToolDecision = "allow_once" | "allow_always" | "deny";

/** One Baker conversation's display facts, straight off `/studio/state`. */
export type BakerThreadInfo = { main: boolean; title: string; archived: boolean };

/** `GET /baker/threads` row / thread-route response. */
export type BakerThreadView = {
  stream_id: string;
  agent_id: string;
  main: boolean;
  title: string;
  archived: boolean;
  busy: boolean;
  last_ts: string;
  model_id: string | null;
};

export type CreateBakerThreadResp = {
  thread: BakerThreadView;
  /** The first message's durable queue record, when one was given. */
  queued: BakeryEvent | null;
};

export type RootView = { root_id: string; path: string; is_git: boolean };

/** One Project with its live roots and current member sockets (07d-A3).
 *  Grouping is by a socket's own `socket.project.set` fact alone, never by
 *  `repo_root` — two sockets can share a Project despite pointing at
 *  entirely different roots (or none). */
export type ProjectStacks = {
  toolchain: "none" | "mise" | "proto";
  tasks: "none" | "moon";
  containers: "none" | "colima" | "orbstack";
  secrets: "bakery" | "infisical";
};

export function defaultProjectStacks(): ProjectStacks {
  return { toolchain: "none", tasks: "none", containers: "none", secrets: "bakery" };
}

export type ProjectGroup = {
  project_id: string;
  name: string;
  pinned: boolean;
  roots: RootView[];
  sockets: SocketInfo[];
  stacks: ProjectStacks;
};

/** `GET /studio/state` — the one topology source (07d-A3 §2): grouped only
 *  by valid `socket.project.set`, never `repo_root`. Ungrouped is real
 *  (sockets with no — or no longer resolvable — project_id); factory
 *  sockets are never in `projects` or `ungrouped_sockets`. */
export type StudioState = {
  model_options: ModelOption[];
  role_options: RoleOption[];
  projects: ProjectGroup[];
  ungrouped_sockets: SocketInfo[];
  factory_sockets: SocketInfo[];
  /** Picture revision. The same number means the floor did not change. */
  revision?: number;
};

export type ProjectView = {
  project_id: string;
  name: string;
  pinned: boolean;
  roots: RootView[];
  stacks: ProjectStacks;
};

/** Explicit attach-existing vs. create-new intent — mirrors
 *  `crate::routes::project_catalog::CreateProjectRoot`'s `#[serde(tag =
 *  "mode", ...)]` shape 1:1, the same way `ActivityPhase` above mirrors its
 *  Rust counterpart. Replaces the old ambiguous `{path, create_if_missing}`,
 *  which let a "create new folder" request against an already-existing path
 *  silently attach to it instead of erroring. */
export type CreateProjectRootArgs =
  /** A fresh folder inside Bakery's own — the usual choice, no path. */
  | { mode: "bakery" }
  | { mode: "attach"; path: string }
  | { mode: "create"; parent: string; name: string };

export type CreateProjectResp = { project: ProjectView; socket: ActiveContext };

/** Each event's payload, parsed once: events are immutable records, and a
 *  thread's transcript is rebuilt from every one of them on each change.
 *  Callers only read what this returns. */
const payloads = new WeakMap<BakeryEvent, Record<string, any>>();

export function payloadOf(ev: BakeryEvent): Record<string, any> {
  let p = payloads.get(ev);
  if (p === undefined) {
    try {
      p = JSON.parse(ev.payload) as Record<string, any>;
    } catch {
      p = {};
    }
    payloads.set(ev, p);
  }
  return p;
}
