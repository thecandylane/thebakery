// GENERATED from crates/bakery-node/src/api_catalog.rs — do not edit.
// Regenerate: BAKERY_WRITE_API_CLIENT=1 cargo test -p bakery-node api_client
// (cargo test fails while this file is out of date).

import { request } from "./apiTransport.ts";

export type Method = "GET" | "POST";
export type Risk = "read" | "write" | "run" | "destructive";

// ── Shared types ──

export type NodeInfo = {
  node_id: string;
  hostname: string;
  stream_fallback: StreamFallback;
  model: string;
  write_mode: WriteMode;
  worktree_base: string;
  max_tool_steps?: number | null;
  /**
   * The checkout of Bakery's own source this node runs from, when known:
   * the Bakery's own repo, as opposed to the operator's projects.
   */
  bakery_source_root?: string | null;
};

/**
 * Where a stream should live when the local machine is not the only option.
 * Stored, surfaced, and honoured only as far as `local_only` semantics today —
 * the remote arms of this enum are *declared*, not implemented.
 */
export type StreamFallback = "local_only" | "prefer_local" | "prefer_server" | "auto";

/**
 * How a proposed `agent.file.changed` reaches **disk** (write gate).
 *
 * Per-node today (see `NodeConfig::write_mode`) — same file, same load/save
 * path as `stream_fallback`; a per-stream override is future work.
 *
 * **Orthogonal to AGENT_CAPABILITY.md § modes** (Manual / Plan / Auto Edit /
 * Auto Go): those gate *capability breadth* (tools, commands, autonomy of
 * the agent loop). `WriteMode` only gates *when a file proposal hits disk*.
 * Compose: e.g. Auto Go + Manual write = full tools but human accept on
 * files; Manual capability + Auto write is unusual and still evented.
 */
export type WriteMode = "manual" | "rules" | "auto";

export type Report = {
  build: Build;
  components: Array<Component>;
  sessions: Array<Session>;
  checked_at?: string | null;
  check_error?: string | null;
};

export type Build = {
  started_at?: string | null;
  source_root?: string | null;
  branch?: string | null;
  commit?: string | null;
  uncommitted_files?: number | null;
  /**
   * A Rust or Cargo file changed after this node started, so the code that
   * runs is older than the checkout until the next restart.
   */
  changed_since_start?: string | null;
};

export type Component = {
  id: string;
  label: string;
  installed?: string | null;
  /** What Bakery pins or requires, when it constrains this at all. */
  required?: string | null;
  /** From the last update check; `None` until one ran. */
  latest?: string | null;
  status: Status;
  /** Where `installed` was read, or why it couldn't be. */
  source: string;
  update: string;
};

export type Status = "ok" | "update_available" | "missing" | "mismatch";

export type Session = {
  stream_id: string;
  seat: string;
  account_id: string;
  /** The runtime's own name and version from its ACP handshake. */
  runtime?: string | null;
  version?: string | null;
  model?: string | null;
  /**
   * Started on an adapter other than the one installed now; it keeps
   * running that one until the session or the node restarts.
   */
  outdated: boolean;
};

export type AgentsView = {
  agents: Array<Agent>;
  tool_policy: Array<KindPolicy>;
  write_mode: WriteMode;
  /** What a container seat gets of this Mac unless set per agent. */
  seat_default: SeatSize;
};

export type Agent = {
  stream_id: string;
  seat: string;
  agent_id: string;
  socket: string;
  scope: string;
  model_id?: string | null;
  /**
   * `native` (its own agent loop, sandboxed), `api` (Bakery's tool
   * loop), or `none` (no model assigned).
   */
  transport: string;
  busy: boolean;
  /**
   * What the runtime reported about itself when it started — name and
   * version, sign-in, models, effort, modes, slash commands, context.
   * `None` while no runtime is running for this agent.
   */
  runtime?: AcpRuntimeSnapshot | null;
  bakery_tools: Array<Tool>;
  sandbox?: Sandbox | null;
  /** Its place on the scope dial (Settings → Agents). */
  reach: Reach;
  /** Whether it may use containers, through Bakery's container tools. */
  containers: boolean;
  /**
   * Its container seat's CPUs and memory, when the operator set them;
   * `None` is the default (`AgentsView::seat_default`).
   */
  seat_size?: SeatSize | null;
  /** Its calls through the model proxy; `None` before its first. */
  model_usage?: AgentUsage | null;
};

/**
 * Credential-free projection of one native ACP session.  This is the only
 * shape allowed across the node/client boundary: adapter argv/env, terminal
 * login commands, and auth metadata that may contain account identifiers are
 * intentionally absent.
 */
export type AcpRuntimeSnapshot = {
  account_id: string;
  generation: number;
  runtime?: AcpRuntimeIdentity | null;
  auth_methods: Array<AcpAuthMethod>;
  auth_status?: AcpAuthStatus | null;
  config_options: Array<AcpConfigOption>;
  modes: Array<AcpMode>;
  current_mode?: string | null;
  commands: Array<AcpCommand>;
  prompt: AcpPromptCapabilities;
  steering: boolean;
  /**
   * The adapter advertised Bakery's narrow reviewed-input extension for
   * native `Edit`/`Write`. This is not an ACP-wide claim: absent means
   * Bakery only exposes the adapter's ordinary allow/reject controls.
   */
  native_review: boolean;
  requested_model?: string | null;
  effective_model?: string | null;
  usage?: AcpUsage | null;
  provenance: string;
};

export type AcpRuntimeIdentity = {
  name: string;
  title?: string | null;
  version: string;
};

export type AcpAuthMethod = {
  id: string;
  name: string;
  description?: string | null;
  /**
   * `agent`, `terminal`, or `environment`.  The latter is reported as a
   * capability but Bakery never accepts its secret value from React.
   */
  kind: string;
};

export type AcpAuthStatus = {
  kind: string;
  label?: string | null;
  detail?: string | null;
  plan?: string | null;
};

export type AcpConfigOption = {
  id: string;
  name: string;
  description?: string | null;
  category?: string | null;
  kind: string;
  /** Untyped JSON: no schema on the node describes it yet. */
  current_value: unknown;
  values: Array<AcpConfigValueOption>;
};

export type AcpConfigValueOption = {
  id: string;
  name: string;
  description?: string | null;
  group?: string | null;
  group_name?: string | null;
};

export type AcpMode = {
  id: string;
  name: string;
  description?: string | null;
};

export type AcpCommand = {
  name: string;
  description: string;
  input_hint?: string | null;
};

export type AcpPromptCapabilities = {
  image: boolean;
  audio: boolean;
  embedded_context: boolean;
};

export type AcpUsage = {
  used: number;
  size: number;
};

export type Tool = {
  name: string;
  summary: string;
  read_only: boolean;
};

export type Sandbox = {
  home: string;
  /** Everywhere outside the kernel's own system paths this runtime can write. */
  writes: Array<string>;
  /** Extra folders it can read (beyond its workspace and home). */
  reads: Array<string>;
  /** Extra folders it can run programs from. */
  runs: Array<string>;
  /** Project folders it can read but never change. */
  never_writes: Array<string>;
};

/** One conversation's place on the scope dial, narrowest last. */
export type Reach = "host_full" | "host_room" | "container" | "locked";

/** A container seat's share of this Mac. */
export type SeatSize = {
  cpus: number;
  memory_gib: number;
};

/** One agent's calls through the model proxy, all time. */
export type AgentUsage = {
  calls: number;
  /** Calls that failed: refused, rate-limited, or the provider erred. */
  failed: number;
  tokens_in: number;
  tokens_out: number;
  /** Calls a model other than the one asked for answered. */
  pin_mismatches: number;
  /** When its last call went through (RFC 3339). */
  last_at: string;
};

export type KindPolicy = {
  kind: ChildToolKind;
  /** `always`, `ask`, or `never`. */
  policy: string;
  /** Read-only kinds are never asked about: `always` unless set `never`. */
  read_only: boolean;
};

export type ChildToolKind = "read" | "search" | "think" | "fetch" | "edit" | "delete" | "move" | "execute" | "switch_mode" | "other";

export type SetPolicy = {
  kind: ChildToolKind;
  policy: string;
};

/** A grant as the operator sees it, with its project's name. */
export type GrantView = {
  scope: GrantScope;
  /** The project (scope project) or the conversation (scope chat). */
  id?: string | null;
  kind: ChildToolKind;
  /** The project's name (scope project). */
  name?: string | null;
};

/** Where an approval card's "Always allow" holds. */
export type GrantScope = "project" | "baker" | "chat";

/** One "Always allow": a kind of action, allowed without asking in one scope. */
export type AlwaysGrant = {
  scope: GrantScope;
  /** The project (scope project) or the conversation (scope chat). */
  id?: string | null;
  kind: ChildToolKind;
};

export type PeerLimit = {
  /** The most open peers agents may open in one project, or one workspace. */
  limit: number;
};

export type CreateChatReq = {
  title?: string;
  /**
   * What its agents may use of the Bakery's tools: all (the default),
   * read or none.
   */
  tools?: ToolsLimit | null;
};

/**
 * What an agent may use of the Bakery's tools: chosen when it is created,
 * changed only by the operator.
 */
export type ToolsLimit = "all" | "read" | "none";

/** What is open — the sticky context bar's whole content. */
export type ActiveContext = {
  stream_id: string;
  name: string;
  /** Repository (project) name. */
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  /** False when the stream is known to the Heart but its directory is gone. */
  exists_on_disk: boolean;
  /**
   * True when `worktree_path` IS the repo's own working tree — the human
   * opened a checkout directly rather than materialising a linked worktree.
   */
  in_place: boolean;
  owner_node_id?: string | null;
  /**
   * True only when *this node* ran `git worktree add` to create
   * `worktree_path` (set once, on the original `project.onboarded` event —
   * never re-derived). A manually attached worktree, or one adopted from
   * disk with no Heart record of how it got there, is `false`: proven
   * provenance, not inferred from where the path happens to sit
   * (04-LIFECYCLE audit: "Bakery ownership is inferred, not proven").
   */
  created_by_bakery: boolean;
};

export type MakeProjectReq = {
  stream_id: string;
  /** The project's name; the chat's title when absent. */
  name?: string | null;
  /**
   * The folder, absolute, that the project's folder goes in; Bakery's own
   * projects folder when absent.
   */
  parent?: string | null;
};

export type MadeProject = {
  project_id: string;
  name: string;
  /** Where the chat's files are now: the project's root. */
  folder: string;
};

export type ArchiveReq = {
  stream_id: string;
  archived: boolean;
};

export type RenameConversationReq = {
  stream_id: string;
  /** Its new name: the first line, clipped. */
  title: string;
};

export type StreamReq = {
  stream_id: string;
};

export type StorageView = {
  agents: Array<AgentStorage>;
  /** Folders shared by every agent on one account. */
  shared: Array<Area>;
  /** The only agent-related writes that happen outside the sandbox. */
  host_side: Array<HostNote>;
  never_in_projects: Array<string>;
};

export type AgentStorage = {
  stream_id: string;
  seat: string;
  socket: string;
  scope: string;
  model_id?: string | null;
  running: boolean;
  areas: Array<Area>;
  /**
   * Agent folders and instruction files present at the project root —
   * yours, readable by agents, never writable by a runtime.
   */
  project_files: Array<string>;
};

export type Area = {
  label: string;
  path: string;
  /** Deleting inside it is allowed (never while its runtime runs). */
  clearable: boolean;
  exists: boolean;
  bytes: number;
  files: number;
  newest?: string | null;
  truncated: boolean;
};

export type HostNote = {
  what: string;
  detail: string;
};

export type PathQuery = {
  path: string;
};

export type Entry = {
  name: string;
  dir: boolean;
  bytes: number;
  modified?: string | null;
  /** Named like a credential: listed, never readable here. */
  credential: boolean;
};

export type FileView = {
  text: string;
  bytes: number;
  truncated: boolean;
  binary: boolean;
};

export type View = {
  policy: MachineAccess;
  presets: Array<Preset>;
  /** Denied under every grant, emitted after every allow. */
  always_denied: Array<string>;
  /** The hosts every container seat may reach; the operator adds more. */
  seat_hosts: Array<string>;
  /** Native runtimes running now — started under the grants in force then. */
  live: Array<LiveRuntime>;
  /**
   * Granted folders that are gone: kept, and granted to no one until they
   * are back.
   */
  missing: Array<string>;
  /**
   * Granted folders that now resolve to what is never granted (a link to
   * a secret store, say), by path, with why: kept, and granted to no one.
   */
  blocked: Record<string, string>;
  /** The operator's home, which the screen shows as `~`. */
  home: string;
};

export type MachineAccess = {
  /** For the Baker, for every project, or both (each grant's `scope`). */
  grants?: Array<Grant>;
  env?: Array<EnvVar>;
  /**
   * One project's own environment, on top of the every-project grants:
   * only that project's agents and tasks get it. Keyed by project id.
   */
  projects?: Record<string, ProjectAccess>;
  /**
   * The operator's own additions to the deny list, denied at every level
   * like the built-in list, which no setting removes.
   */
  deny?: Array<string>;
  /**
   * Each conversation's place on the dial, by stream id; absent means its
   * default: [`Reach::HostFull`] for the Baker and the Bakery's own
   * workspaces, else its project's kind (`project_reach`). Set per agent
   * (`POST /machine-access/reach`), never by saving the policy.
   */
  reach?: Record<string, Reach>;
  /**
   * Conversations whose agent may use containers, by stream id; off by
   * default at every reach. Only through Bakery's container tools
   * (`crate::containers::agent_may`), never a Docker socket or Apple
   * container's own CLI. Set per agent (`POST /machine-access/containers`),
   * never by saving the policy.
   */
  containers?: Array<string>;
  /**
   * Hosts container seats may also reach through their gateway, beyond
   * the built-in ones (`seat_gateway::BUILT_IN`): the operator's own
   * additions, which never remove a built-in one. A leading dot allows
   * every subdomain.
   */
  allow_hosts?: Array<string>;
  /**
   * Each container seat's CPUs and memory, by stream id; absent means the
   * default sized from this Mac (`seat_container::seat_default`). Set per
   * agent (`POST /machine-access/seat-size`), never by saving the policy.
   */
  seat_size?: Record<string, SeatSize>;
};

export type Grant = {
  path: string;
  access: Access;
  scope?: Scope;
  /** Put this directory on the runtime's PATH (run grants only). */
  on_path?: boolean;
  /** The preset that added it, for display; `None` = added by hand. */
  preset?: string | null;
};

export type Access = "read" | "write" | "exec" | "full";

/**
 * Who a grant reaches: the Baker, the other agents (project agents and task
 * workers), or both — each chosen independently.
 */
export type Scope = "baker" | "agents" | "all_agents";

export type EnvVar = {
  name: string;
  value: string;
  scope?: Scope;
  preset?: string | null;
};

export type ProjectAccess = {
  grants?: Array<Grant>;
  env?: Array<EnvVar>;
};

export type Preset = {
  id: string;
  label: string;
  detail: string;
  grants: Array<Grant>;
  env: Array<EnvVar>;
  /** Why it can't be offered on this machine, when it can't. */
  unavailable?: string | null;
};

export type LiveRuntime = {
  stream_id: string;
  busy: boolean;
  baker: boolean;
};

export type Saved = {
  policy: MachineAccess;
  presets: Array<Preset>;
  /** Denied under every grant, emitted after every allow. */
  always_denied: Array<string>;
  /** The hosts every container seat may reach; the operator adds more. */
  seat_hosts: Array<string>;
  /** Native runtimes running now — started under the grants in force then. */
  live: Array<LiveRuntime>;
  /**
   * Granted folders that are gone: kept, and granted to no one until they
   * are back.
   */
  missing: Array<string>;
  /**
   * Granted folders that now resolve to what is never granted (a link to
   * a secret store, say), by path, with why: kept, and granted to no one.
   */
  blocked: Record<string, string>;
  /** The operator's home, which the screen shows as `~`. */
  home: string;
  /** Idle runtimes stopped so their next turn starts under the new grants. */
  restarted: Array<string>;
  /**
   * Mid-turn runtimes; each restarts under the new grants when its turn
   * ends.
   */
  busy: Array<string>;
};

export type CheckReq = {
  scope?: Scope;
  /** Check as an agent of this project (its own environment included). */
  project_id?: string | null;
};

export type CheckResult = {
  tools: Array<ToolProbe>;
  guards: Array<GuardProbe>;
  path: string;
};

export type ToolProbe = {
  name: string;
  works: boolean;
  /** The file that actually ran — e.g. Apple's /usr/bin/python3 or yours. */
  path: string;
  /** Its first line of output — the version, or why it didn't run. */
  detail: string;
};

export type GuardProbe = {
  what: string;
  /** `denied` (held), `absent` (nothing there to protect), or `allowed`. */
  result: string;
  held: boolean;
};

export type DecideAccess = {
  stream_id: string;
  event_hash: string;
  allow: boolean;
};

/**
 * The shape of a Heart event (`bakery_core::Event`, which carries no
 * schema of its own), for the schemas of responses that hold one.
 */
export type EventShape = {
  stream_id: string;
  seq: number;
  content_hash: string;
  prev_hash?: string | null;
  actor: string;
  ts: string;
  causality: Array<string>;
  layer?: string | null;
  sensitivity: string;
  family: string;
  /** The event's payload, itself JSON text. */
  payload: string;
  event_hash: string;
};

export type ReachReq = {
  stream_id: string;
  reach: Reach;
};

export type ReachSet = {
  reach: Reach;
  /**
   * False while a turn runs: the runtime restarts under it when the turn
   * ends.
   */
  restarted: boolean;
};

export type ContainersReq = {
  stream_id: string;
  on: boolean;
};

export type ContainersSet = {
  on: boolean;
  /** False while a turn runs: the runtime restarts when the turn ends. */
  restarted: boolean;
};

export type SeatSizeReq = {
  stream_id: string;
  /** `None` returns the seat to the default sized from this Mac. */
  size?: SeatSize | null;
};

export type SeatSizeSet = {
  /** What the seat gets from its next start. */
  size: SeatSize;
  /** False while a turn runs: the runtime restarts when the turn ends. */
  restarted: boolean;
};

/**
 * The wire projection of a [`TerminalHandle`] — everything Studio needs to
 * render a terminal tab and its status, nothing about the process itself.
 */
export type TerminalInfo = {
  terminal_id: string;
  stream_id: string;
  name: string;
  mode: string;
  cwd: string;
  live: boolean;
  created_ts: string;
  /**
   * Who opened it: "human" for the operator, an agent's actor
   * ("agent:<seat>", or "agent:<thread>:<seat>" from another thread), or
   * "agent" for the stream's Agent terminal, which shows every agent's
   * commands.
   */
  opened_by: string;
};

export type SeatImage = {
  /** The image seats run in. */
  image: string;
  state: ImageState;
};

/** Whether the seat image is built from this node's recipe. */
export type ImageState = "missing" | "current" | "stale";

export type ListQuery = {
  backend: Backend;
  kind: Kind;
};

export type Backend = "apple" | "docker" | "orbstack";

export type Kind = "containers" | "images" | "volumes" | "networks" | "compose";

/** One kind's rows on one backend; the other kinds' lists are empty. */
export type Listing = {
  backend: Backend;
  kind: Kind;
  containers: Array<ContainerRow>;
  images: Array<ImageRow>;
  volumes: Array<VolumeRow>;
  networks: Array<NetworkRow>;
  compose: Array<ComposeRow>;
};

export type ContainerRow = {
  id: string;
  name: string;
  image: string;
  running: boolean;
  /** The backend's own words, e.g. "Up 4 hours (healthy)" or "running". */
  status: string;
  ports: Array<Port>;
  /**
   * Its address on its network, where the backend lists it (Apple
   * container).
   */
  address?: string | null;
  labels: Record<string, string>;
  /** Set when Bakery started it. */
  role?: Role | null;
  /** The conversation it belongs to (a seat's or an editor's). */
  stream_id?: string | null;
  /** The agent's seat in that conversation. */
  seat?: string | null;
  /** Who asked for a run: "human", or an agent ("agent:<seat>"). */
  started_by?: string | null;
};

/** A port a container publishes on this Mac, or only exposes. */
export type Port = {
  /** On this Mac; `None` when the port is only exposed. */
  host?: number | null;
  container: number;
  protocol: string;
};

/** What Bakery started a container as, from its labels. */
export type Role = "seat" | "editor" | "run";

export type ImageRow = {
  id: string;
  /** `repository:tag`, or the id when it has none. */
  name: string;
  /** e.g. "438MB" (Docker's words) or "824 MB". */
  size: string;
  /** e.g. "2 weeks ago" or "2026-09-30". */
  created: string;
};

export type VolumeRow = {
  name: string;
  driver: string;
  /**
   * Docker's own words; on Apple container, its disk image's capacity,
   * not what is used.
   */
  size: string;
  labels: Record<string, string>;
};

export type NetworkRow = {
  id: string;
  name: string;
  /** e.g. "bridge", or Apple container's "nat" and "hostOnly". */
  driver: string;
  /** No route out of this Mac (Docker's internal, Apple's host-only). */
  internal: boolean;
  subnet?: string | null;
  gateway?: string | null;
  labels: Record<string, string>;
};

export type ComposeRow = {
  name: string;
  /** e.g. "running(3)". */
  status: string;
  files: Array<string>;
};

export type ItemQuery = {
  backend: Backend;
  kind: Kind;
  /** Name or id. */
  id: string;
};

export type LogsQuery = {
  backend: Backend;
  /** Container name or id. */
  id: string;
  /** Last lines to show (default 200, at most 5000). */
  tail?: number | null;
};

export type BackendQuery = {
  backend: Backend;
};

export type ContainerReq = {
  backend: Backend;
  /** Container name or id. */
  id: string;
};

export type ItemReq = {
  backend: Backend;
  kind: Kind;
  /** Name or id (a project's name for `compose`). */
  id: string;
};

export type KindReq = {
  backend: Backend;
  kind: Kind;
};

export type ComposeUpReq = {
  /** The compose file to bring up. */
  file: string;
  /** docker (Colima's default VM, the default) or orbstack. */
  backend?: Backend | null;
};

export type RunReq = {
  backend: Backend;
  /** The image, e.g. `docker.io/library/postgres:17`. */
  image: string;
  /** The container's name; the backend picks one when absent. */
  name?: string | null;
  /** The command and its arguments, in place of the image's own. */
  command?: Array<string>;
  mounts?: Array<MountReq>;
  ports?: Array<PortReq>;
  env?: Record<string, string>;
  /**
   * The project it's for, when the operator runs it (an agent's run is
   * its own project's): its ports are that project's.
   */
  project_id?: string | null;
};

export type MountReq = {
  /** A host folder, absolute (a file too, on Docker). */
  source: string;
  /** Where the container sees it, absolute. */
  target: string;
  /** Read and write; read-only otherwise. */
  write?: boolean;
};

export type PortReq = {
  /**
   * The port on this Mac, published on 127.0.0.1 only, inside the
   * Bakery's block (46000–46499). Leave it out: the Bakery picks the
   * project's own, the same each time, which only that project's rooms
   * reach.
   */
  host?: number | null;
  /** The container's port. */
  container: number;
};

export type Ran = {
  ok: boolean;
  /** The new container's id. */
  id: string;
  /** Its ports, as this Mac publishes them on 127.0.0.1. */
  published: Array<PublishedPort>;
};

export type PublishedPort = {
  host: number;
  container: number;
};

export type CodeReq = {
  stream_id: string;
};

export type ViewUrl = {
  /** A 127.0.0.1 URL, for this Mac's own views. */
  url: string;
};

export type Closed = {
  /** Whether the conversation's editor was open. */
  closed: boolean;
};

export type AppearanceReq = {
  /** Whether the scheme's surfaces are dark. */
  dark: boolean;
  /**
   * The scheme's colors as the app resolves them, by token
   * (`--bg-app`: `#0d0e11` or `rgb(…)`).
   */
  colors: Record<string, string>;
  /** The scheme's code font, when it sets one (`--font-mono`). */
  mono_font?: string | null;
};

export type GotoReq = {
  stream_id: string;
  /** The edit row's change (`agent.file.changed`). */
  event_hash: string;
};

export type Goto = {
  /** The conversation's editor: load it into its Code view, unless `opened`. */
  url: string;
  /**
   * The editor already showing in its Code view opened the file there:
   * bring that view forward.
   */
  opened: boolean;
  /** The file, as the editor names it. */
  file: string;
  /** The first line the edit changed, where the cursor goes. */
  line: number;
  /** The file no longer reads as the edit left it: changed since, or undone. */
  changed: boolean;
};

export type PreviewReq = {
  stream_id: string;
  /**
   * The port the dev server listens on, in the conversation's container
   * or on this Mac.
   */
  port: number;
};

/** What the node knows about the proxy now. */
export type ProxyStatus = {
  installed: boolean;
  version: string;
  running: boolean;
  healthy: boolean;
  port: number;
  /** Why it isn't running, or what last stopped it. */
  error?: string | null;
  /** The hosts its upstream calls may reach. */
  egress_hosts: Array<string>;
};

/** Which providers the proxy holds keys for, and how many; never a key. */
export type ModelKeys = {
  providers: Array<ProviderKeys>;
};

export type ProviderKeys = {
  provider: Provider;
  keys: Array<KeyView>;
};

/** A provider whose API keys the proxy holds. */
export type Provider = "claude" | "gemini" | "xai";

export type KeyView = {
  /** Its fingerprint: names the key without carrying it. */
  id: string;
  label: string;
  added: string;
};

/** An API key the operator gives the proxy. (No Debug: it holds a key.) */
export type AddKey = {
  provider: Provider;
  /** The API key itself; it goes to the proxy and nowhere else. */
  key: string;
  /** The operator's name for it. */
  label?: string;
};

export type RemoveKey = {
  provider: Provider;
  /** The key's fingerprint, from GET /model-proxy/keys. */
  id: string;
};

export type AgentMap = {
  nodes: Array<MapNode>;
};

export type MapNode = {
  id: string;
  /** "baker" | "thread" | "task" | "chat" | "project" | "subagent" */
  kind: string;
  label: string;
  /** The model it runs on, or a subagent's type. */
  detail?: string | null;
  /** "working" | "needs_input" | "failed" | "done" | "idle" | "stopped" */
  status: string;
  note?: string | null;
  /** The node that started it; none for a top-level agent. */
  parent?: string | null;
  stream_id: string;
  seat: string;
  /** A subagent's launching tool call. */
  tool_call_id?: string | null;
  /** The tool calls a subagent made so far. */
  steps: number;
  last_ts: string;
};

export type ControlStatus = {
  deployed?: DeployedBuild | null;
  /** What Roll back would switch to. */
  previous?: DeployedBuild | null;
  /** This node runs the deployed build, not one built from source. */
  running_deployed: boolean;
  /**
   * Source files changed after the deployed build began, the first
   * [`UNDEPLOYED_LISTED`] of them; empty when nothing is deployed.
   */
  undeployed: Array<string>;
  undeployed_count: number;
  last?: LastAction | null;
  /**
   * A restart (a deploy's, say) waiting for running turns, with
   * `POST /control/restart/now` to go ahead at once.
   */
  restart?: RestartWait | null;
};

/** `build.json`, as the script writes it when a deploy begins. */
export type DeployedBuild = {
  commit?: string | null;
  built_at?: string | null;
  /** Uncommitted files the build was made with. */
  dirty_files?: number | null;
};

export type LastAction = {
  action: string;
  started_at: string;
  running: boolean;
  /**
   * The script's own verdict: `None` while it runs, or when it ended
   * without giving one (killed outright).
   */
  ok?: boolean | null;
  /**
   * Why, when it failed: `upgrade-adapter` exits 3 when the Bakery patch
   * does not apply to the new release and needs porting.
   */
  exit_code?: number | null;
  log_tail: Array<string>;
};

export type RestartWait = {
  /** When the restart was asked for (RFC 3339). */
  since: string;
  /** Turns it still waits for. */
  turns: number;
  /** Seconds until it goes ahead anyway. */
  seconds_left: number;
};

export type ControlReq = {
  /**
   * `restart`, `deploy`, `deploy-ui` (only the UI is built and swapped in:
   * the open app reloads, nothing restarts), `rollback` or `upgrade-adapter`.
   */
  action: string;
};

/** Amphetamine as it stands. */
export type Amphetamine = {
  installed: boolean;
  /** A session keeps this Mac awake. */
  active: boolean;
  /** Seconds left in a timed session; none for one with no end. */
  seconds_left?: number | null;
  /** Closed-display mode: awake with the lid shut. */
  closed_display: boolean;
};

export type AmphetamineReq = {
  /** `start`, `end`, `closed_display_on` or `closed_display_off`. */
  action: string;
  /** For `start`: minutes; none, or 0, for a session with no end. */
  minutes?: number | null;
};

export type Meters = {
  disk: DiskMeter;
  memory: MemoryMeter;
  cpu: CpuMeter;
  heat: HeatMeter;
  /** One reading every [`METER_EVERY`], the last ten minutes, oldest first. */
  history: Array<MeterSample>;
};

export type DiskMeter = {
  level: MeterLevel;
  free: number;
  total: number;
};

export type MeterLevel = "ok" | "warn" | "bad";

export type MemoryMeter = {
  level: MeterLevel;
  /** What Activity Monitor calls Memory Used. */
  used: number;
  total: number;
  /** `normal`, `warn` or `critical`, as macOS says it. */
  pressure: string;
  swap_used?: number | null;
};

export type CpuMeter = {
  level: MeterLevel;
  /** Across all cores, since the last reading. */
  percent: number;
  load1: number;
  cores: number;
};

export type HeatMeter = {
  level: MeterLevel;
  thermal?: string | null;
  hottest_c?: number | null;
  holding: boolean;
};

export type MeterSample = {
  at: string;
  cpu_percent: number;
  memory_used: number;
};

export type Heat = {
  /** `nominal`, `fair`, `serious` or `critical`, as macOS reports it. */
  thermal?: string | null;
  /** The hottest of its temperature sensors, in °C. */
  hottest_c?: number | null;
  fans: Array<Fan>;
  /** Agents' new turns wait while this Mac runs hot. */
  holding: boolean;
};

/** One fan. */
export type Fan = {
  rpm: number;
  min?: number | null;
  max?: number | null;
};

export type DiskReport = {
  /** The disk the Bakery's data is on. */
  free: number;
  total: number;
  /** Free space is under a tenth of the disk. */
  low: boolean;
  /** Largest first. */
  build_folders: Array<BuildFolder>;
  /** Docker in Colima's default VM, by kind; absent when it isn't running. */
  docker?: Array<DiskUse> | null;
  /**
   * Docker in OrbStack, by kind; absent when it isn't installed or
   * running.
   */
  orbstack?: Array<DiskUse> | null;
  /** Absent when Apple container isn't running. */
  apple_container?: Array<DiskUse> | null;
  /**
   * Every image, container and volume on both, with the project that
   * uses each.
   */
  items: Array<EngineItem>;
  /** What Colima's VMs take on this Mac's disk; absent without Colima. */
  colima?: number | null;
  /** The Bakery's own data, by its top folders, largest first. */
  bakery: Array<DataPart>;
};

export type BuildFolder = {
  /** A thread whose folder holds it, and its name. */
  stream_id: string;
  thread: string;
  path: string;
  /**
   * The tool that makes it: Cargo, Flutter, npm, Gradle, SwiftPM or
   * CocoaPods.
   */
  made_by: string;
  bytes: number;
};

export type DiskUse = {
  /** `images`, `containers`, `volumes` or `build cache`. */
  kind: string;
  count: number;
  /** In use by a container. */
  active: number;
  bytes: number;
  /** What nothing uses. */
  reclaimable: number;
};

export type EngineItem = {
  /** `docker` (Colima's), `orbstack` or `apple`. */
  engine: string;
  /** `image`, `container` or `volume`. */
  kind: string;
  /**
   * What the engine calls it: repository:tag, a container's name, a
   * volume's name.
   */
  name: string;
  /** What removes it: an image's or a container's id, a volume's name. */
  id: string;
  bytes?: number | null;
  /** A container's state; for an image or a volume, `used` or `unused`. */
  state: string;
  /**
   * The Bakery project that uses it, `Bakery` for the Bakery's own; none
   * when nothing tells.
   */
  project?: string | null;
  /** The compose or Supabase project its labels name. */
  group?: string | null;
};

export type DataPart = {
  name: string;
  bytes: number;
};

export type CleanReq = {
  path: string;
  what: "build_folder";
} | {
  backend: Backend;
  what: "unused_images";
} | {
  backend: Backend;
  what: "stopped_containers";
} | {
  backend: Backend;
  what: "build_cache";
} | {
  backend: Backend;
  names: Array<string>;
  what: "volumes";
};

export type Cleaned = {
  /** Free space after. */
  free: number;
  /** What the tool said, for a container cleanup. */
  output: string;
};

export type EngineRoom = {
  /**
   * Apple container's system service: `running`, or what it says
   * instead; absent when it isn't installed.
   */
  apple_container?: string | null;
  /** Absent when Tailscale isn't installed. */
  tailscale?: TailnetStatus | null;
  /** Colima's VMs; absent when Colima isn't installed. */
  colima?: Array<ColimaVm> | null;
  /** What Colima's default VM uses, while it runs. */
  colima_use?: ColimaUse | null;
  /** The most its programs held on any day of the last fortnight. */
  colima_peak?: number | null;
  /** A memory size from that peak, in GiB: half again as much, at least 2. */
  colima_suggested_gib?: number | null;
  /** The size the operator picked, applied at the VM's next start. */
  colima_next_size?: ColimaSize | null;
  /** OrbStack, once installed. */
  orbstack?: OrbStack | null;
  /**
   * The Bakery's Docker engine where a project picks none: `colima` or
   * `orbstack`.
   */
  docker_default: string;
  /** Colima's idle stop is on (unless the operator turned it off). */
  colima_idle_stop: boolean;
  /** The idle stop stopped Colima's default VM: a Docker action starts it. */
  colima_idle_stopped: boolean;
  brew_services: Array<BrewService>;
  /**
   * cloudflared runs, as a Homebrew service or on its own; absent when it
   * isn't installed.
   */
  cloudflared?: boolean | null;
  /** Supabase's local stacks running in Colima, by project. */
  supabase: Array<string>;
};

export type TailnetStatus = {
  connected: boolean;
  /** This Mac's name on the tailnet. */
  name?: string | null;
  /** The other machines and phones, those online first. */
  peers: Array<TailnetPeer>;
};

export type TailnetPeer = {
  /** Its name on the tailnet. */
  name: string;
  /** `iOS`, `android`, `linux`, `macOS`, `windows`… */
  os: string;
  online: boolean;
};

export type ColimaVm = {
  /** Its profile: `default` is the one the Bakery's Docker uses. */
  name: string;
  running: boolean;
  cpus?: number | null;
  /** Bytes. */
  memory?: number | null;
};

export type ColimaUse = {
  /** The VM's memory as its own Linux counts it. */
  guest_total: number;
  /** What its programs hold: neither free nor cache. */
  guest_programs: number;
  /** Its page cache, which it gives up when programs need more. */
  guest_cache: number;
  guest_free: number;
  /** Each running container's own use. */
  containers: Array<ContainerUse>;
  /**
   * What the VM holds of this Mac's memory (its process's footprint), which
   * only a stop gives back.
   */
  on_mac?: number | null;
};

export type ContainerUse = {
  name: string;
  /** Bytes. */
  memory: number;
  cpu_percent: number;
};

/** The size the operator picked for Colima's default VM. */
export type ColimaSize = {
  memory_gib: number;
  /** None keeps its CPUs. */
  cpus?: number | null;
};

export type OrbStack = {
  /** Its Docker answers, through its `orbstack` context. */
  running: boolean;
  /**
   * The memory and CPUs its Docker sees: OrbStack grows into that memory
   * as its containers need it and gives back to macOS what they free.
   */
  memory?: number | null;
  cpus?: number | null;
  /** Each running container's own use. */
  containers: Array<ContainerUse>;
  /** What OrbStack holds of this Mac's memory now. */
  on_mac?: number | null;
};

export type BrewService = {
  name: string;
  /** As Homebrew says: `started`, `stopped`, `error`, `none`… */
  status: string;
};

export type IdleStopReq = {
  on: boolean;
};

export type DockerDefaultReq = {
  /** `colima` or `orbstack`. */
  engine: string;
};

export type DockerDefault = {
  engine: string;
  /**
   * What the switch means for what's already there; none when nothing
   * changed.
   */
  note?: string | null;
};

export type ColimaSizeReq = {
  /**
   * The size for the VM's next start; none takes back a size not yet
   * applied.
   */
  size?: ColimaSize | null;
};

export type ServiceCopyReq = {
  stream_id: string;
  /** `redis` or `postgres`. */
  service: string;
};

/** A thread's separate copy of one of its project's services. */
export type ServiceCopy = {
  stream_id: string;
  service: string;
  port: number;
  /** Where its thread reaches it, any password left out. */
  url: string;
  /** When it started (RFC 3339). */
  since: string;
};

export type ServiceStopReq = {
  stream_id: string;
  port: number;
};

export type SecretFilesQuery = {
  project_id: string;
};

export type SecretFiles = {
  project_id: string;
  /** The operator's answers, allowed and denied, by path. */
  files: Array<FileReadAnswer>;
  asks: Array<FileReadAsk>;
};

/** One of the operator's answers. */
export type FileReadAnswer = {
  path: string;
  allow: boolean;
  at: string;
};

/** A file waiting on the operator. */
export type FileReadAsk = {
  /** From the thread's folder (`frontend/assets/pub.pem`). */
  path: string;
  /** What the node says it is. */
  kind: SecretKind;
  bytes: number;
  /** The program that was refused it (`dartvm`), as macOS names it. */
  program?: string | null;
  /** The thread that asked. */
  stream_id: string;
  /** The ask's event in that thread: its card. */
  event_hash: string;
  at: string;
};

/** What the node says a secret-named file is. */
export type SecretKind = "public_key" | "certificate" | "private_key" | "unknown" | "token_or_env";

export type FileReadDecide = {
  project_id: string;
  /** From the thread's folder, as the ask names it. */
  path: string;
  /**
   * Let the project's builds read it, or not; `null` forgets the answer,
   * so it asks again.
   */
  allow?: boolean | null;
  /**
   * For a file the node says is a private key: the operator's second
   * yes.
   */
  sure?: boolean;
};

/**
 * Something the app reports about itself (`POST /client/log`): an uncaught
 * error, a React error, a navigation that never finished.
 */
export type ClientLog = {
  kind: string;
  message: string;
  detail?: string;
  path?: string;
};

export type WriteModeReq = {
  write_mode: WriteMode;
  /**
   * Optional: stream the human was looking at. Always audit; if omitted,
   * events land on `node:policy` so StatusBar clicks are not silent.
   */
  stream_id?: string | null;
};

export type InspectReq = {
  path: string;
};

export type InspectResp = {
  project: ProjectInfo;
  /** Free worktree name derived from the repo basename. */
  suggested_name: string;
  /**
   * Free branch name for `suggested_name` — lets the wizard render an exact
   * preview (name, branch, path) before anything is created.
   */
  suggested_branch: string;
  /**
   * This node's worktree base directory — the wizard shows it as the
   * WORKTREE PATH preview, distinct from the source repo path.
   */
  worktree_base: string;
  /** Streams this node already has for this repository. */
  bakery_streams: Array<StreamRef>;
};

/** Everything the wizard needs to offer a branch + worktree choice. */
export type ProjectInfo = {
  /** The path the human picked, as given. */
  path: string;
  is_git: boolean;
  /** Why `is_git` is false (or a git failure). `None` when fine. */
  error?: string | null;
  /** Absolute toplevel of the *main* working tree (not the picked worktree). */
  root?: string | null;
  /** Basename of `root` — the project name shown in the context bar. */
  name?: string | null;
  /** Branch checked out at `path`, or `None` when detached. */
  current_branch?: string | null;
  /** `origin/HEAD` → else main → else master → else current. */
  default_branch?: string | null;
  local_branches: Array<string>;
  worktrees: Array<WorktreeInfo>;
};

export type WorktreeInfo = {
  path: string;
  /** Short branch name, or `None` when detached/bare. */
  branch?: string | null;
  head?: string | null;
  /** The repository's own working tree (first entry of `git worktree list`). */
  is_main: boolean;
  locked: boolean;
};

/** A stream as "recents" needs it: enough to re-open with one click. */
export type StreamRef = {
  stream_id: string;
  /** The name it was opened with, which reopening it goes by. */
  name: string;
  /** The name the operator gave it since, shown in place of `name`. */
  title?: string | null;
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  exists_on_disk: boolean;
  /**
   * True when `worktree_path` IS the repo's own working tree — the human
   * opened a checkout directly rather than materialising a linked worktree.
   */
  in_place: boolean;
  owner_node_id?: string | null;
  last_ts: string;
  events: number;
  /**
   * `stream.lifetime.set`'s latest value, or `Slice` when no such event
   * exists yet (STEERING: "slice — default for 'new session on this bug'").
   */
  lifetime: Lifetime;
  /**
   * True once `stream.discarded` has landed — worktree/room are gone,
   * Heart lineage is not.
   */
  discarded: boolean;
};

/**
 * A stream's declared lifetime (STEERING.md § Sockets). Purely a Heart fact
 * — nothing in this crate ever assigns `Living` or `Burn` on its own; both
 * require an explicit `POST /stream/lifetime` call. A stream with no
 * `stream.lifetime.set` event defaults to `Slice`, matching STEERING's
 * "slice — default for 'new session on this bug'".
 */
export type Lifetime = "living" | "slice" | "burn";

/**
 * The Open wizard's request. Everything except `repo` is optional: the node
 * suggests the name, picks the default branch, and decides create-vs-attach.
 */
export type OpenReq = {
  /** Any path inside the repository — the node resolves the real root. */
  repo: string;
  /** Worktree name; also the branch suffix and stream key. Auto-suggested when absent. */
  name?: string | null;
  /**
   * Branch to check out in the worktree. Defaults to a free branch derived
   * from `{name}` (see `crate::git::suggest_branch`) — not a `bakery/`-prefixed one.
   */
  branch?: string | null;
  /** Branch to fork from when creating `branch`. Defaults to the detected default branch. */
  base_branch?: string | null;
  /** Attach this already-materialised worktree instead of running `git worktree add`. */
  worktree_path?: string | null;
  /**
   * 07d-A3: bind the resulting socket to this Project as one of its roots
   * (root path = the socket's own `worktree_path`, the actual working
   * directory — never the bare `repo_root`, since a Project's root is
   * "a working directory", not "a git repository", and one root may be
   * shared by more than one socket/worktree). `None` preserves today's
   * behavior exactly: a bare git-worktree open with no Project membership
   * (Ungrouped) — "today's open/worktree path becomes this capability,
   * not creating the project."
   */
  project_id?: string | null;
  /**
   * What a new conversation's agents may use of the Bakery's tools: all
   * (the default), read or none.
   */
  tools?: ToolsLimit | null;
};

export type OpenResp = {
  /** The stream's `project.onboarded` event (existing one when attaching). */
  event: EventShape;
  context: ActiveContext;
  /** True when this call attached to a stream that was already onboarded. */
  attached: boolean;
};

export type CreateProjectReq = {
  name: string;
  /** `None` = chat-only (no root at creation time). */
  root?: CreateProjectRoot | null;
  stacks?: ProjectStacks;
  /**
   * What its first conversation's agents may use of the Bakery's tools:
   * all (the default), read or none.
   */
  tools?: ToolsLimit | null;
};

/**
 * Explicit attach-existing vs. create-new directory-root intent (replaces
 * the old ambiguous `path` + `create_if_missing: bool`, which let a
 * "create new" request against an already-existing path silently attach
 * to it instead of erroring). Mirrors `catalog::DirectoryRootSpec`
 * structurally — `PreparedRoot::spec` below is the only place that converts between
 * them, so the wire shape and the internal enum can never drift apart
 * unnoticed.
 */
export type CreateProjectRoot = {
  path: string;
  mode: "attach";
} | {
  parent: string;
  name: string;
  mode: "create";
} | {
  mode: "bakery";
};

/**
 * Payload of `project.created` — the one immutable fact of a Project's
 * birth. `name` here is never overwritten; `project.renamed` records later
 * choices, but a malformed/unreadable rename falls back to this original
 * name (not to an older rename) precisely because it is structurally
 * privileged: set once, verified once, never itself subject to "newest
 * wins" ambiguity.
 */
export type ProjectStacks = {
  /** Toolchain version manager: `none` | `mise` | `proto`. */
  toolchain?: string;
  /** Task runner: `none` | `moon`. */
  tasks?: string;
  /**
   * Where its services run, the setup's "Services run in": `apple`
   * ("Apple container", on its project's own network), `orbstack`
   * ("OrbStack") or `colima` ("Colima"). Its Docker stacks run on that
   * engine when it's one, else on the Bakery's default (`engine`). An
   * old `none` reads as `apple`.
   */
  containers?: string;
  /** Secrets: `bakery` | `infisical`. */
  secrets?: string;
  /**
   * Where its agents run, the setup's "Agents run in": `mac` ("Mac
   * sandbox", `mac_sandbox`) or `container` ("Apple container", a Linux
   * seat on its project's own network). An old `none` reads as `mac`.
   */
  sandbox?: string;
  /** Its own services (`services`): `redis`. None at first. */
  services?: Array<string>;
  /**
   * The sign-ins its rooms may use through their door (`swap`): each
   * gets a stand-in, never the credential. None at first.
   */
  sign_ins?: Array<string>;
};

export type CreateProjectResp = {
  project: ProjectView;
  socket: ActiveContext;
};

export type ProjectView = {
  project_id: string;
  name: string;
  pinned: boolean;
  roots: Array<RootView>;
  stacks: ProjectStacks;
};

export type RootView = {
  root_id: string;
  path: string;
  is_git: boolean;
};

export type RenameProjectReq = {
  project_id: string;
  name: string;
};

export type ControlQuery = {
  /**
   * The project; "" for the Bakery's own, read from its main checkout
   * and the worktrees its own conversations work in.
   */
  project_id: string;
};

export type ControlPlane = {
  project_id: string;
  name: string;
  folders: Array<Folder>;
  mcp: Array<McpServer>;
  skills: Array<Skill>;
  instructions: Array<Instruction>;
  /** The Bakery's own setup for the project, which the operator may change. */
  setup: Setup;
  /**
   * What its own toolchain copies hold that the operator's don't (a Mac
   * sandbox project's), each of which the operator may keep for good.
   */
  additions: Array<Addition>;
  /**
   * Every install its agents asked for, with the operator's answer and
   * how it went.
   */
  installs: Array<Install>;
  /**
   * The secret-named files the operator let its builds read, or not, and
   * the ones its rooms were refused that wait on them.
   */
  secret_files?: SecretFiles | null;
  /** Its own services (its setup names them), as they run. */
  services: Array<ServiceRunning>;
  /** The separate copies its threads started for destructive work. */
  service_copies: Array<ServiceCopy>;
  /**
   * The folders beyond its own its agents reach, which the operator adds
   * (Settings → Machine access, for this project); none at first.
   */
  extra_folders: Array<Grant>;
};

export type Folder = {
  /** The folder, one of the project's roots. */
  path: string;
  name: string;
  stack: Array<StackItem>;
  dependencies: Array<Dependency>;
  tools: Array<Tool2>;
};

/** One thing a folder is built with. */
export type StackItem = {
  kind: Kind2;
  name: string;
  version?: string | null;
  /** The file that says so, relative to its folder. */
  source: string;
};

export type Kind2 = "language" | "framework" | "platform" | "toolchain" | "ci" | "check";

/** One direct dependency, as its manifest names it. */
export type Dependency = {
  name: string;
  version?: string | null;
  /** For development only (tests, builds, linting). */
  dev: boolean;
  source: string;
};

/** A command-line tool a folder relies on. */
export type Tool2 = {
  name: string;
  source: string;
};

export type McpServer = {
  name: string;
  /** How it runs: a command and its arguments, or a URL. */
  command?: string | null;
  args: Array<string>;
  url?: string | null;
  /** The names its environment and its headers set; never their values. */
  env: Array<string>;
  headers: Array<string>;
  /**
   * The file that defines it, relative to `folder`; none for the
   * Bakery's own, which every Bakery agent gets.
   */
  source?: string | null;
  folder?: string | null;
};

export type Skill = {
  name: string;
  description?: string | null;
  /** Each copy's `SKILL.md`, with its folder. */
  files: Array<string>;
  /**
   * The skill folders that hold it: `.claude/skills` (Claude reads it),
   * `.agents/skills` (other agents do).
   */
  dirs: Array<string>;
};

export type Instruction = {
  folder: string;
  /** `AGENTS.md`, `CLAUDE.md` or `GEMINI.md`. */
  file: string;
  bytes: number;
};

export type Setup = {
  current: ProjectStacks;
  suggestions: Array<Suggestion>;
};

/** A setting a folder's files point to, different from the current one. */
export type Suggestion = {
  /** `toolchain`, `tasks`, `containers`, `secrets` or `sandbox`. */
  field: string;
  value: string;
  /** The file that suggests it, with its folder. */
  because: string;
};

/** One thing the project's own copies hold that the operator's don't. */
export type Addition = {
  /** `flutter`, `mise` or `gem`. */
  tool: string;
  /** What it is: `flutter`, a mise tool (`node`), or a gem's name. */
  name: string;
  version: string;
};

/** One install, as History and the control plane show it. */
export type Install = {
  tool: string;
  name: string;
  version: string;
  reason: string;
  /** The thread that asked. */
  stream_id: string;
  /** The ask's event in that thread: its card. */
  event_hash: string;
  asked_at: string;
  /** The operator's answer, once given. */
  allow?: boolean | null;
  answered_at?: string | null;
  /** Whether it installed, once done, with the last of its output. */
  ok?: boolean | null;
  detail?: string | null;
  done_at?: string | null;
};

/** One of a project's services, as it runs. */
export type ServiceRunning = {
  service: string;
  port: number;
  /** `redis://127.0.0.1:<port>`, or Postgres's without its password. */
  url: string;
  running: boolean;
};

export type SetupReq = {
  /**
   * The project; "" for the Bakery's own, which its own conversations
   * follow (where its agents run stays Machine access's).
   */
  project_id: string;
  setup: ProjectStacks;
};

export type HostsQuery = {
  /** The project; "" for the Bakery's own, as its own threads meet it. */
  project_id: string;
};

export type Hosts = {
  project_id: string;
  /**
   * Each host once, as it stands now: defaults first (the operator's
   * answer in a default's place), then the operator's other answers,
   * then what the project's files point to.
   */
  hosts: Array<Host>;
  asks: Array<Ask>;
};

/** One line of a project's list. */
export type Host = {
  /** A host, or `*.domain` for any host under that domain. */
  host: string;
  allow: boolean;
  source: Source;
  /** The file that points to it (`pubspec.yaml`), for `project`. */
  because?: string | null;
  /** When the operator answered, for `operator`. */
  at?: string | null;
};

export type Source = "default" | "project" | "operator";

/** A host waiting on the operator. */
export type Ask = {
  host: string;
  port: number;
  /** The thread that asked. */
  stream_id: string;
  /** The ask's event in that thread: its card. */
  event_hash: string;
  at: string;
  /**
   * The program that tried, as it names itself (`Dart/3.9`), when it
   * said.
   */
  program?: string | null;
};

export type Decide = {
  /** The project; "" for the Bakery's own, which its own threads meet. */
  project_id: string;
  /** A host, or `*.domain` for any host under that domain. */
  host: string;
  /**
   * Allow or deny it for the project; `null` forgets the answer, so the
   * host asks again.
   */
  allow?: boolean | null;
};

/** One thing the project's own copies hold that the operator's don't. */
export type KeepReq = {
  project_id: string;
  /** `flutter`, `mise` or `gem`. */
  tool: string;
  /** What it is: `flutter`, a mise tool (`node`), or a gem's name. */
  name: string;
  version: string;
};

export type InstallReq = {
  stream_id: string;
  /**
   * `flutter` (a Flutter version, through FVM), `mise` (a mise tool's
   * version) or `gem`.
   */
  tool: string;
  /** The mise tool or the gem; `flutter` for Flutter. */
  name: string;
  version: string;
  /** Why the project needs it, for the operator. */
  reason?: string;
};

export type InstallDecide = {
  project_id: string;
  /** The ask's event in its thread, as its card and the list name it. */
  event_hash: string;
  allow: boolean;
};

export type DoorQuery = {
  stream_id: string;
};

export type DoorUrl = {
  /**
   * `http://127.0.0.1:<port>`: a proxy for the thread's Preview, held to
   * its project's list.
   */
  url: string;
};

export type ToolchainsQuery = {
  stream_id: string;
};

export type Toolchains = {
  /** The thread's own folder. */
  folder: string;
  toolchains: Array<Toolchain>;
};

export type Toolchain = {
  /** `mise`, `rustup`, `FVM` or `Xcode`. */
  manager: string;
  /** `node`, `python`, `Rust`, `Flutter`, `Xcode`… */
  tool: string;
  /** The version the folder pins; none when it pins none. */
  wanted?: string | null;
  /** The file that pins it, relative to the folder. */
  source?: string | null;
  /** What the thread's room runs; none when it runs none. */
  found?: string | null;
  ready: boolean;
  /** Why not, as the tool said it. */
  why?: string | null;
};

export type IphoneBuildQuery = {
  stream_id: string;
};

export type IphoneBuild = {
  /** The newest unsigned iPhone build the button would install, if any. */
  build?: IphoneBuilt | null;
};

export type IphoneBuilt = {
  /** The app's folder name (`Runner.app`). */
  app: string;
  /** When the thread built it (RFC 3339). */
  built: string;
};

export type IphoneRunReq = {
  stream_id: string;
  /**
   * The iPhone, by its name or identifier; when left out, the one iPhone
   * paired with this Mac.
   */
  device?: string | null;
};

export type IphoneRunPrepared = {
  /** The operator's own terminal (not a room) where it waits for Enter. */
  terminal_id: string;
  command: string;
  device: string;
};

export type IphoneInstallReq = {
  stream_id: string;
  /**
   * The iPhone, by its name or identifier; when left out, the one iPhone
   * paired with this Mac.
   */
  device?: string | null;
};

export type IphoneInstalled = {
  /** The iPhone it's on now. */
  device: string;
  bundle_id: string;
  /** When the thread built it (RFC 3339). */
  built: string;
  /** The provisioning profile it's signed for. */
  profile: string;
};

export type PinProjectReq = {
  project_id: string;
  pinned: boolean;
};

export type RetireProjectReq = {
  project_id: string;
};

/**
 * "Open any directory as a root + in-place socket" on an existing Project —
 * no git worktree machinery, no admission requirement beyond "the path
 * resolves to a directory" (07d-A3 §1).
 */
export type OpenRootReq = {
  project_id: string;
  name: string;
  root: CreateProjectRoot;
  /**
   * What its agents may use of the Bakery's tools: all (the default),
   * read or none.
   */
  tools?: ToolsLimit | null;
};

/**
 * Re-parent (or first-attach) an ALREADY-OPEN socket to a Project — the
 * socket's own recorded `worktree_path`/`repo_root` (from its
 * `project.onboarded` event) supplies the root path and git-ness; the
 * caller never re-states them.
 */
export type AttachSocketReq = {
  stream_id: string;
  project_id: string;
};

export type DetachSocketReq = {
  stream_id: string;
};

export type RollupQuery = {
  project_id: string;
};

export type ProjectRollup = {
  project_id: string;
  repos: Array<RepoRollup>;
  worktrees: Array<WorktreeRollup>;
};

export type RepoRollup = {
  /** The repo's own working tree; each worktree names it as its `repo`. */
  path: string;
  /** Its open pull requests, newest first. */
  prs: Array<PullRequest>;
  /** The latest run of each workflow on each branch, newest first. */
  runs: Array<Run>;
  /**
   * Why GitHub couldn't answer for this repo (gh missing or signed out,
   * no GitHub remote); the rest of the rollup still did.
   */
  problem?: string | null;
};

export type PullRequest = {
  number: number;
  title: string;
  branch: string;
  draft: boolean;
  /** pass, fail, pending, or none (no checks). */
  checks: string;
  /** APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED, or empty. */
  review: string;
  updated_at: string;
  url: string;
};

export type Run = {
  branch: string;
  workflow: string;
  title: string;
  /** queued, in_progress or completed. */
  status: string;
  /** success, failure, cancelled…; empty until it completes. */
  conclusion: string;
  created_at: string;
  url: string;
};

export type WorktreeRollup = {
  stream_id: string;
  /** The repo it belongs to (a `repos[].path`). */
  repo?: string | null;
  /** None on a detached HEAD. */
  branch?: string | null;
  /**
   * The branch it is counted against: the one it forked from, or its
   * repo's main for a worktree Bakery didn't make (or made before it kept
   * a record of this).
   */
  base?: string | null;
  added: number;
  removed: number;
  /** It has uncommitted changes. */
  dirty: boolean;
  /**
   * Commits on its base it doesn't have yet (rule 3: "N behind"),
   * counted as of `fetched`.
   */
  behind: number;
  /**
   * When its repository was last fetched, and why it failed if it did;
   * none since the node started.
   */
  fetched?: Fetched | null;
  /**
   * Other open threads on its repository changing some of the same files
   * (git rule 5), as of the last turn's end.
   */
  also_changing: Array<Overlap>;
};

/**
 * When a repository was last fetched, and why it failed if it did: the
 * "fetched 2 min ago" beside "N behind".
 */
export type Fetched = {
  at: string;
  error?: string | null;
};

/** Another thread changing some of the same files. */
export type Overlap = {
  stream_id: string;
  /** The files both have changed since they forked, committed or not. */
  files: Array<string>;
};

/**
 * One row of the cross-stream "waiting on you" queue.
 *
 * Deliberately carries NO file bodies. The client previously built this
 * queue by fetching every stream's full history and diffing `old_content`
 * against `new_content` in the browser — N round trips, each one shipping
 * whole files just to render `+3 / −1`. The counts are computed here and
 * the bytes stay on the node; a client that wants the bytes still opens the
 * stream and reviews the proposal itself.
 */
export type Gate = {
  /**
   * Only `"write"` is produced today. Elevated and handoff gates have no
   * data source yet (NODE_API.md §8, §9) and are never synthesized here.
   */
  kind: string;
  stream_id: string;
  /** The pending `agent.file.changed` — the hash `POST /agent/accept` wants. */
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

export type HeartQuery = {
  /** `1` / `true` opts into the full chain verification. */
  verify?: string | null;
};

/**
 * What the Heart actually knows about itself.
 *
 * Every field here is measured, never modelled. There are no projection
 * counts and no staleness figure because there are no projections: the
 * store is one append-only `events` table. A dashboard that wants L0–L4
 * rows has to wait for layers to exist rather than be handed placeholders.
 */
export type HeartStats = {
  /** Streams with at least one event. */
  streams: number;
  /** Total events across every stream. */
  events: number;
  /** Oldest and newest event timestamps in the store; `None` when empty. */
  first_ts?: string | null;
  last_ts?: string | null;
  /**
   * On-disk footprint: `heart.db` plus its `-wal`/`-shm` sidecars.
   * `None` when the Heart is in-memory (tests) or the files cannot be
   * stat'ed — never 0, which would read as "empty" rather than "unknown".
   */
  db_bytes?: number | null;
  /**
   * Present only when the caller asked for `?verify=1`. Verification
   * re-reads and re-hashes every event, so it is opt-in: a stats poll
   * must not silently become an O(all events) scan, and this endpoint
   * must not claim "chain verified" on a check it never ran.
   */
  chain?: ChainCheck | null;
};

export type ChainCheck = {
  /** True only when every stream's hash chain verified end to end. */
  verified: boolean;
  streams_checked: number;
  /**
   * Streams whose chain did not verify, with the reason. Empty when
   * `verified` — tamper is always named, never reduced to a count.
   */
  broken: Array<BrokenChain>;
};

export type BrokenChain = {
  stream_id: string;
  reason: string;
};

export type StreamQuery = {
  stream_id: string;
  /**
   * Accepted and ignored: per-seat actors do not exist yet (NODE_API §2),
   * so there is nothing seat-specific to scope a context view to. Taking
   * the parameter keeps the client's URL stable for when they do.
   */
  seat?: string | null;
};

/**
 * What "memory" is in this node today.
 *
 * - `hot`: real projection `agent_tool_turn` injects (tail scan).
 * - `entries`: durable L1 notes from `memory.written` events only
 * (BRAIN_MAP / brain.md six-month filter — caller discipline).
 * L2–L4 remain empty until those layers exist — never invent graph rows.
 */
export type MemoryView = {
  stream_id: string;
  /** Durable L1 entries projected from `memory.written` (latest per id). */
  entries: Array<MemoryEntry>;
  hot: HotMemoryView;
};

/**
 * A note's shape, as the Bakery keeps it for every runtime: one fact a
 * note, named, typed, described in one line, linked to others by name.
 * Every field is optional: a plain note is just its text.
 */
export type MemoryEntry = {
  id: string;
  layer: string;
  sensitivity: string;
  text: string;
  written_at: string;
  by: string;
  event_hash: string;
  /** Its name, which other notes link to as `[[name]]`. */
  name?: string | null;
  /** user | feedback | project | reference */
  type?: string | null;
  /** One line: what it holds, for the index and for deciding relevance. */
  description?: string | null;
  /**
   * Where it came from: the thread that wrote it, or the file it was
   * imported from.
   */
  source?: string | null;
  /** Why it holds: what happened that taught it. */
  why?: string | null;
  /** How to act on it. */
  how_to_apply?: string | null;
  /** The notes its text links to, as `[[name]]`. */
  links: Array<string>;
};

export type HotMemoryView = {
  /** Compact prior-session facts, oldest first, as injected. */
  lines: Array<string>;
  /** Recently touched paths, newest last. */
  paths: Array<string>;
  prior_accept_path?: string | null;
  /** Policy that produced the above, so a reader can see the window. */
  scan_events: number;
  max_lines: number;
};

/**
 * A note's shape, as the Bakery keeps it for every runtime: one fact a
 * note, named, typed, described in one line, linked to others by name.
 * Every field is optional: a plain note is just its text.
 */
export type MemoryWriteReq = {
  stream_id: string;
  /** Durable note body. Empty → refuse. */
  text: string;
  /** Stable page id (brain.md-style). Auto-minted when omitted. */
  id?: string | null;
  /** Only `L1` accepted today; other layers → refuse (no invented L2–L4). */
  layer?: string | null;
  /** `shared` | `private` | `canon` — default `shared`. */
  sensitivity?: string | null;
  /** Its name, which other notes link to as `[[name]]`. */
  name?: string | null;
  /** user | feedback | project | reference */
  type?: string | null;
  /** One line: what it holds, for the index and for deciding relevance. */
  description?: string | null;
  /**
   * Where it came from: the thread that wrote it, or the file it was
   * imported from.
   */
  source?: string | null;
  /** Why it holds: what happened that taught it. */
  why?: string | null;
  /** How to act on it. */
  how_to_apply?: string | null;
};

export type ToolkitQuery = {
  /** That project's items and the Bakery-wide ones; every item when absent. */
  project_id?: string | null;
};

export type Toolkit = {
  items: Array<ToolkitView>;
};

export type ToolkitView = {
  id: string;
  kind: Kind3;
  /** Its name: a skill's or an MCP server's, as agents call it. */
  name: string;
  /** One line: what it is for, and when to use it. */
  description?: string;
  /** The project it belongs to; Bakery-wide when absent. */
  project_id?: string | null;
  enabled: boolean;
  /** A skill's or instructions' text; a hook's command. */
  text?: string;
  /** An MCP server or custom tool run as a program: its command and arguments. */
  command?: string | null;
  args?: Array<string>;
  /** An MCP server over HTTP: its address. */
  url?: string | null;
  /** Hooks: when it fires — `pre_tool` or `post_tool`. Inert when absent. */
  event?: string | null;
  /**
   * Hooks: which tool names to match. Empty or `*` = all. Pipe-separated
   * list for multiples (`Edit|Write`). Otherwise exact name.
   */
  matcher?: string | null;
  /**
   * Custom tools: JSON Schema of its parameters.
   * Untyped JSON: no schema on the node describes it yet.
   */
  params?: unknown;
  /**
   * Plugins: what Claude Code knows it by, and what it holds. Its files
   * are kept in [`plugin_dir`].
   */
  plugin?: PluginRecord | null;
  runtimes: Array<Carriage>;
};

export type Kind3 = "skill" | "mcp" | "hook" | "plugin" | "instructions" | "tool" | "command" | "agent" | "output_style";

/** A plugin carried whole, as Claude Code installed it. */
export type PluginRecord = {
  /** `<name>@<marketplace>`, as Claude Code's settings name it. */
  id: string;
  version: string;
  /**
   * Its marketplace's source, as Claude Code records it.
   * Untyped JSON: no schema on the node describes it yet.
   */
  marketplace_source: unknown;
  /**
   * Its entry in that marketplace's catalog.
   * Untyped JSON: no schema on the node describes it yet.
   */
  entry: unknown;
  parts: PluginParts;
};

/** What a plugin holds, by name, so its parts can go to other runtimes. */
export type PluginParts = {
  skills: Array<string>;
  commands: Array<string>;
  agents: Array<string>;
  /** The hook events it registers. */
  hooks: Array<string>;
  mcp_servers: Array<string>;
};

export type Carriage = {
  /** claude, codex, grok, gemini, api (the models the Bakery calls), local */
  runtime: string;
  /** carried (as it is), translated (into its own form), or not_yet */
  how: string;
  note: string;
};

export type SetReq = {
  /** The item to change; a new one when absent. */
  id?: string | null;
  kind: Kind3;
  name: string;
  description?: string;
  project_id?: string | null;
  /** On unless said otherwise. */
  enabled?: boolean | null;
  text?: string;
  command?: string | null;
  args?: Array<string>;
  url?: string | null;
  event?: string | null;
  matcher?: string | null;
  /** Untyped JSON: no schema on the node describes it yet. */
  params?: unknown;
};

export type IdReq = {
  id: string;
};

export type EnableReq = {
  id: string;
  enabled: boolean;
};

export type PortQuery = {
  /**
   * The scope it would come into: that project's, or Bakery-wide when
   * absent.
   */
  project_id?: string | null;
};

export type Port2 = {
  found: Array<Found>;
};

export type Found = {
  /** What the import names it by. */
  key: string;
  /**
   * What it is in Claude Code: `skill`, `mcp_server`, `hook`,
   * `instructions`, `rule`, `plugin`, `command`, `agent` or `output_style`.
   */
  found: string;
  /** What it becomes in the Toolkit; none when it can't come across yet. */
  kind?: Kind3 | null;
  name: string;
  description: string;
  /** Where it is, from the home folder. */
  source: string;
  /** Why it can't come across, when it can't. */
  reason?: string | null;
  /** The keys and secrets left out, by name only. */
  left_out: Array<string>;
  /** What changes on the way, or won't work on the other side. */
  warnings: Array<string>;
  /** The Toolkit item it would change, in the scope asked about. */
  in_toolkit?: string | null;
  /** A plugin's parts. */
  parts?: PluginParts | null;
};

export type ImportReq = {
  /** What to bring, by the keys the list gave. */
  keys: Array<string>;
  /** Into that project; Bakery-wide when absent. */
  project_id?: string | null;
};

export type ImportResult = {
  imported: Array<Imported>;
};

export type Imported = {
  key: string;
  /** The Toolkit item it became, or changed. */
  id?: string | null;
  error?: string | null;
};

export type Learning = {
  settings: LoopSettings;
  changed_at?: string | null;
  /** operator (none: never changed). */
  changed_by?: string | null;
  /** When the nightly run next starts; none while it is off or paused. */
  next_nightly?: string | null;
  /** Why parts won't run now. */
  held: Array<string>;
  /** The models the loop may ask. */
  models: Array<LoopModel>;
  last: LastRuns;
};

/** Everything about the loop the operator decides. */
export type LoopSettings = {
  /** Nothing runs on its own while paused; run now still runs. */
  paused?: boolean;
  /**
   * The model reflection and replays run on; they wait until one is
   * picked.
   */
  reviewer_model?: string | null;
  /** Projects left out of the loop, by id. */
  projects_out?: Array<string>;
  thread_end?: ThreadEnd;
  nightly?: Nightly;
  reflect?: Reflect;
  replay?: Replay;
  lift?: Lift;
  digest?: Digest;
};

export type ThreadEnd = {
  on?: boolean;
  /** When a thread is archived. */
  on_archive?: boolean;
  /** When a thread's work is merged. */
  on_merge?: boolean;
  /** At the nightly run, for each thread that worked that day. */
  in_nightly?: boolean;
  /** The fewest turns since its last review that make a thread worth one. */
  min_turns?: number;
  /** The most notes one review proposes. */
  max_proposals?: number;
  /** The model it runs on; the thread's own when absent. */
  model?: string | null;
};

export type Nightly = {
  on?: boolean;
  /** When it starts, local time (HH:MM). */
  at?: string;
  /** Until when it may still start, retrying while the Mac is busy or hot. */
  retry_until?: string;
  retry_every_minutes?: number;
  /** It waits while any agent's turn runs. */
  wait_while_busy?: boolean;
  /** It waits while the Mac is hot (agents' new turns wait then too). */
  wait_while_hot?: boolean;
};

export type Reflect = {
  on?: boolean;
  /**
   * How far back it looks for one kind of work that both passed and
   * failed.
   */
  window_days?: number;
  /** The most lessons one run proposes. */
  max_proposals?: number;
};

export type Replay = {
  on?: boolean;
  /** The most lessons replayed in one run. */
  cap?: number;
  /** Which lessons first: newest, oldest, most_used or least_proven. */
  order?: string;
};

export type Lift = {
  on?: boolean;
  /**
   * In how many projects alike lessons must be before they are proposed
   * for the whole Bakery.
   */
  min_projects?: number;
};

export type Digest = {
  on?: boolean;
  /** A Mac notification with each digest. */
  notify?: boolean;
};

export type LoopModel = {
  id: string;
  label: string;
  /** http, acp or openai-compat. */
  runtime: string;
  available: boolean;
  reason?: string | null;
};

/** Each part's last run. */
export type LastRuns = {
  thread_end?: LearningRun | null;
  nightly?: LearningRun | null;
  reflect?: LearningRun | null;
  replay?: LearningRun | null;
  lift?: LearningRun | null;
  digest?: LearningRun | null;
};

/** One run of one part of the loop. */
export type LearningRun = {
  id?: string;
  /** thread_end, nightly, reflect, replay, lift or digest. */
  part?: string;
  /** archive, merge, nightly or run_now. */
  trigger?: string;
  started_at?: string;
  ended_at?: string | null;
  /** running, done, skipped or failed. */
  status?: string;
  /** What it did, or why it didn't. */
  said?: string;
  thread?: string | null;
  project?: string | null;
  /** The notes it touched. */
  notes?: Array<string>;
  /** The proposals it made. */
  proposals?: Array<string>;
  model_calls?: Array<ModelCall>;
  /**
   * What its scratch threads asked for and were refused: a push, a pull
   * request, a new site, an install, a question for the operator.
   */
  refused?: Array<string>;
};

export type ModelCall = {
  model: string;
  /** What it was asked for. */
  purpose: string;
  /** The thread it was about. */
  thread?: string | null;
  tokens_in?: number | null;
  tokens_out?: number | null;
  ok: boolean;
};

export type RunReq2 = {
  /** thread_end, nightly, reflect, replay, lift or digest. */
  part: string;
  /** The thread a thread-end review reads. */
  thread?: string | null;
  /**
   * Only this project's threads, kinds of work and lessons; every
   * project in the loop when absent.
   */
  project?: string | null;
};

export type RunsQuery = {
  /** Only this part's runs. */
  part?: string | null;
  /** At most this many; 100 when absent. */
  limit?: number | null;
};

export type LearningRuns = {
  /** The newest first. */
  runs: Array<LearningRun>;
};

export type MemorySearchReq = {
  /** The words to look for. */
  query: string;
  /** Only these types: user, feedback, project, reference, skill, lesson. */
  types?: Array<string>;
  /**
   * Only these scopes: bakery, baker, project, workspace, or a whole one
   * (project:<id>).
   */
  scopes?: Array<string>;
  /** At most this many notes; 8 when absent. */
  k?: number | null;
  /** At most about this many tokens of answer; 800 when absent. */
  budget_tokens?: number | null;
};

export type MemoryFound = {
  /** The best first. */
  notes: Array<NoteLine>;
  /** More matched than k or the budget let in. */
  more: boolean;
};

/** A note in one line, as a search finds it. */
export type NoteLine = {
  id: string;
  type: string;
  scope: string;
  title: string;
  summary: string;
  /** How many facts or files it rests on. */
  evidence: number;
  stale: boolean;
};

export type MemoryGetReq = {
  ids: Array<string>;
  /** At most about this many tokens of answer; 2000 when absent. */
  budget_tokens?: number | null;
};

export type MemoryGot = {
  notes: Array<NoteView>;
  /** The note whose text was cut to fit the budget. */
  cut?: string | null;
  /** Past the budget: ask for these again. */
  left_out: Array<string>;
  /** No note you see. */
  unknown: Array<string>;
};

export type NoteView = {
  id?: string;
  /** What `[[name]]` links call it. */
  name?: string;
  /**
   * user, feedback, project, reference, skill or lesson; empty for a
   * plain note.
   */
  type?: string;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope?: string;
  title?: string;
  /** One line. */
  summary?: string;
  body?: string;
  why?: string;
  /** How to apply it, with {variables}. */
  how_to_apply?: string;
  links?: Array<NoteLink>;
  /**
   * What it rests on: Heart facts by hash, or files in its thread's
   * workspace with their hash (`{path, hash}`).
   */
  evidence?: Array<unknown>;
  /** operator or agent: who wrote it, or proposed it. */
  trust?: string;
  /**
   * The thread that proposed it, when the operator kept it from a
   * proposal.
   */
  proposed_by?: string | null;
  /**
   * Where it came from, when not written here: the runtime memory file
   * it was taken in from ("Claude's memory: <file>.md"), or the file it
   * was imported from.
   */
  source?: string | null;
  /** The note it replaces. */
  supersedes?: string | null;
  valid_from?: string;
  valid_to?: string | null;
  /** kept, or scratch: an agent's own, straight to its thread, unreviewed. */
  status?: string;
  /** 1 for the first, one more with each change. */
  version?: number;
  /** The thread it was written on. */
  stream_id?: string;
  /** This version's Heart fact. */
  event_hash?: string;
  written_at?: string;
  /** operator, or the thread of the agent that wrote it. */
  written_by?: string;
  uses: NoteUses;
  /**
   * Something it rests on moved or changed, a kept note supersedes it
   * (merged into another), or its time ran out.
   */
  stale: boolean;
};

export type NoteLink = {
  /** The note it points at, by name or id. */
  to: string;
  /** mention (a `[[name]]` in its text), link, or supersedes. */
  kind: string;
};

/** How a note is used. */
export type NoteUses = {
  /** Agents' whole reads of it in the last seven days, and ever. */
  read_week: number;
  read_total: number;
  /** The turns that used it (it rode on their message, or they read it). */
  turns: number;
  /**
   * The turns that used it and went well (finished, their work
   * accepted) or badly (failed, out of steps, their work rejected or
   * corrected by the operator), and agents' ratings.
   */
  helped: number;
  hurt: number;
  last_used?: string | null;
  /** The thread that last used it. */
  last_used_by?: string | null;
};

export type MemoryRelatedReq = {
  id: string;
  /** How many links away: 1 or 2; 1 when absent. */
  hops?: number | null;
};

export type MemoryRelated = {
  notes: Array<MemoryRelatedLine>;
};

/** A note in one line, as a search finds it. */
export type MemoryRelatedLine = {
  id: string;
  type: string;
  scope: string;
  title: string;
  summary: string;
  /** How many facts or files it rests on. */
  evidence: number;
  stale: boolean;
  /** mention, link or supersedes. */
  link: string;
  /**
   * out: the note asked about (or one nearer it) links to this one; in:
   * this one links to it.
   */
  way: string;
  hops: number;
};

export type MemoryNotesQuery = {
  /** kept (when absent), scratch, or all. */
  status?: string | null;
  /** Only this type. */
  type?: string | null;
  /** Only this scope: bakery, baker, project, workspace, or a whole one. */
  scope?: string | null;
};

export type MemoryNotes = {
  /** The newest first. */
  notes: Array<NoteView>;
};

export type MemoryNoteQuery = {
  id: string;
};

export type NotePage = {
  id?: string;
  /** What `[[name]]` links call it. */
  name?: string;
  /**
   * user, feedback, project, reference, skill or lesson; empty for a
   * plain note.
   */
  type?: string;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope?: string;
  title?: string;
  /** One line. */
  summary?: string;
  body?: string;
  why?: string;
  /** How to apply it, with {variables}. */
  how_to_apply?: string;
  links?: Array<NoteLink>;
  /**
   * What it rests on: Heart facts by hash, or files in its thread's
   * workspace with their hash (`{path, hash}`).
   */
  evidence?: Array<unknown>;
  /** operator or agent: who wrote it, or proposed it. */
  trust?: string;
  /**
   * The thread that proposed it, when the operator kept it from a
   * proposal.
   */
  proposed_by?: string | null;
  /**
   * Where it came from, when not written here: the runtime memory file
   * it was taken in from ("Claude's memory: <file>.md"), or the file it
   * was imported from.
   */
  source?: string | null;
  /** The note it replaces. */
  supersedes?: string | null;
  valid_from?: string;
  valid_to?: string | null;
  /** kept, or scratch: an agent's own, straight to its thread, unreviewed. */
  status?: string;
  /** 1 for the first, one more with each change. */
  version?: number;
  /** The thread it was written on. */
  stream_id?: string;
  /** This version's Heart fact. */
  event_hash?: string;
  written_at?: string;
  /** operator, or the thread of the agent that wrote it. */
  written_by?: string;
  uses: NoteUses;
  /**
   * Something it rests on moved or changed, a kept note supersedes it
   * (merged into another), or its time ran out.
   */
  stale: boolean;
  /** Every version, the newest first. */
  versions: Array<Note>;
};

export type Note = {
  id?: string;
  /** What `[[name]]` links call it. */
  name?: string;
  /**
   * user, feedback, project, reference, skill or lesson; empty for a
   * plain note.
   */
  type?: string;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope?: string;
  title?: string;
  /** One line. */
  summary?: string;
  body?: string;
  why?: string;
  /** How to apply it, with {variables}. */
  how_to_apply?: string;
  links?: Array<NoteLink>;
  /**
   * What it rests on: Heart facts by hash, or files in its thread's
   * workspace with their hash (`{path, hash}`).
   */
  evidence?: Array<unknown>;
  /** operator or agent: who wrote it, or proposed it. */
  trust?: string;
  /**
   * The thread that proposed it, when the operator kept it from a
   * proposal.
   */
  proposed_by?: string | null;
  /**
   * Where it came from, when not written here: the runtime memory file
   * it was taken in from ("Claude's memory: <file>.md"), or the file it
   * was imported from.
   */
  source?: string | null;
  /** The note it replaces. */
  supersedes?: string | null;
  valid_from?: string;
  valid_to?: string | null;
  /** kept, or scratch: an agent's own, straight to its thread, unreviewed. */
  status?: string;
  /** 1 for the first, one more with each change. */
  version?: number;
  /** The thread it was written on. */
  stream_id?: string;
  /** This version's Heart fact. */
  event_hash?: string;
  written_at?: string;
  /** operator, or the thread of the agent that wrote it. */
  written_by?: string;
};

export type ProposeReq = {
  /** user, feedback, project, reference, skill or lesson. */
  type: string;
  title: string;
  /** One line: what it holds, for deciding whether to read it. */
  summary: string;
  /**
   * Plain facts, not orders to yourself ("deploys go through
   * bakery.sh", not "always deploy with bakery.sh"); the steps go in
   * how_to_apply.
   */
  body: string;
  /** What happened that taught it. */
  why?: string;
  /** How to apply it next time, with {variables} where it varies. */
  how_to_apply?: string;
  /**
   * What it rests on, at least one: a file in your workspace
   * ({"path": …}), a Heart fact ({"event": hash}), a commit ({"commit":
   * sha}) or a page ({"url": …}).
   */
  evidence: Array<unknown>;
  /** The note it replaces, by id. */
  supersedes?: string | null;
  /**
   * workspace (your thread only) or project (your project): your project
   * when absent, or your thread when it has none. The operator may widen
   * it.
   */
  scope?: string | null;
};

export type Proposed = {
  /** insert, update or supersede; none when it adds nothing. */
  action: string;
  /** The proposal now waiting for the operator. */
  proposal?: string | null;
  /** The note it changes, or what it repeats. */
  note?: string | null;
  /** What it repeats is kept, waiting or declined. */
  repeats?: string | null;
  said: string;
};

export type RateReq = {
  id: string;
  /** helped or hurt: how the note did in the turn that used it. */
  rating: string;
};

export type Rated = {
  id: string;
  uses: NoteUses;
};

export type ProposalsQuery = {
  /** pending, kept or declined; pending when absent. */
  status?: string | null;
};

export type MemoryProposals = {
  /** The oldest first. */
  proposals: Array<Proposal>;
};

/** A note an agent proposed, and what became of it. */
export type Proposal = {
  id?: string;
  /**
   * insert (a new note), update (a new version of `target`) or supersede
   * (a new note replacing `target`).
   */
  action?: string;
  target?: string | null;
  /** The note as proposed. */
  note?: Note;
  /** The thread that proposed it. */
  proposed_by?: string;
  proposed_at?: string;
  /** pending, kept or declined. */
  status?: string;
  /** The note it became. */
  kept_as?: string | null;
  /** Why the operator declined it. */
  why_declined?: string | null;
  decided_at?: string | null;
};

export type ReviewReq = {
  proposal: string;
  /** keep or decline. */
  decision: string;
  /** What the operator changes as they keep it. */
  edits?: NoteEdits;
  /** Why it is declined. */
  why?: string;
};

/** The operator's changes to a note. */
export type NoteEdits = {
  type?: string | null;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope?: string | null;
  title?: string | null;
  summary?: string | null;
  body?: string | null;
  why?: string | null;
  how_to_apply?: string | null;
  /** When it stops holding. */
  valid_to?: string | null;
};

export type Reviewed = {
  proposal: string;
  /** kept or declined. */
  status: string;
  /** The note it became. */
  note?: string | null;
};

export type KeepAllReq = {
  /** These proposals; every one waiting when empty. */
  proposals?: Array<string>;
};

export type KeptAll = {
  /** Each proposal and the note it became. */
  kept: Array<Reviewed>;
  /** Those not kept, and why. */
  skipped: Array<string>;
};

/** The operator's changes to a note. */
export type EditNoteReq = {
  /**
   * The note to change; a new note of the operator's when absent (then
   * its type, scope, title and body).
   */
  id?: string | null;
  type?: string | null;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope?: string | null;
  title?: string | null;
  summary?: string | null;
  body?: string | null;
  why?: string | null;
  how_to_apply?: string | null;
  /** When it stops holding. */
  valid_to?: string | null;
};

/** The operator's changes to a note. */
export type PromoteReq = {
  /** The scratch note. */
  id: string;
  /** bakery, baker, project:<id> or workspace:<thread>. */
  scope: string | null;
  type?: string | null;
  title?: string | null;
  summary?: string | null;
  body?: string | null;
  why?: string | null;
  how_to_apply?: string | null;
  /** When it stops holding. */
  valid_to?: string | null;
};

export type RetireReq = {
  id: string;
  why?: string;
  /** The note it was merged into. */
  merged_into?: string | null;
};

export type Retired = {
  id: string;
};

export type MemoryDigest = {
  /**
   * Proposals waiting for the operator, the oldest first: Keep all keeps
   * them (POST /memory/keep-all).
   */
  pending: Array<Proposal>;
  /** Kept notes gone stale. */
  stale: Array<NoteLine>;
  /** The last day's moments, the newest first. */
  day: Array<MemoryMoment>;
};

/** One line of the memory's timeline. */
export type MemoryMoment = {
  at: string;
  /** proposed, kept, edited, written, declined or retired. */
  what: string;
  note?: string | null;
  proposal?: string | null;
  /** operator, or the agent's thread. */
  by: string;
  title: string;
};

export type TimelineQuery = {
  /** Only this note's moments. */
  note?: string | null;
  /** At most this many; 200 when absent. */
  limit?: number | null;
};

export type MemoryTimeline = {
  /** The newest first. */
  moments: Array<MemoryMoment>;
};

export type BacklogQuery = {
  /**
   * `bakery`, or a project's id: only its items. Every item you reach
   * when absent.
   */
  scope?: string | null;
};

export type Backlog = {
  /** In the backlog's order. */
  items: Array<BacklogItem>;
};

export type BacklogItem = {
  id: string;
  title: string;
  /** Why it is worth doing. */
  why: string;
  /** The project it is for; the Bakery itself when absent. */
  project_id?: string | null;
  status: Status2;
  /** Its place in the backlog, from 0 at the top. */
  order: number;
  /** `operator`, or the thread of the agent that added it. */
  added_by: string;
  added_at: string;
  /**
   * The threads it concerns: the agent's that added it, any agent's that
   * changed it, and any the operator linked.
   */
  threads: Array<string>;
  /** Each change, oldest first. */
  history: Array<Change>;
};

export type Status2 = "queued" | "doing" | "done" | "dropped";

export type Change = {
  at: string;
  /** `operator`, or the thread of the agent that made it. */
  by: string;
  /** added, edited, moved, or the status it was put in. */
  what: string;
};

export type AddReq = {
  title: string;
  /** Why it is worth doing: what it brings. An agent always says. */
  why?: string;
  /**
   * `bakery`, or a project's id. Absent: an agent's own project (the
   * Bakery when it has none); the Bakery for the operator.
   */
  scope?: string | null;
};

export type EditReq = {
  id: string;
  title?: string | null;
  why?: string | null;
  /** `bakery`, or a project's id. */
  scope?: string | null;
  /** The threads it concerns, by stream id: these replace the ones linked. */
  threads?: Array<string> | null;
};

export type StatusReq = {
  id: string;
  status: Status2;
};

export type MoveReq = {
  id: string;
  /** The item it goes just above; the end of the backlog when absent. */
  before?: string | null;
};

export type IdReq2 = {
  id: string;
};

export type SkillQuery = {
  name: string;
};

export type Skill2 = {
  name: string;
  description: string;
  text: string;
};

export type BriefQuery = {
  stream_id: string;
};

export type Brief = {
  stream_id: string;
  /**
   * What its agent gets every turn: the newest brief not turned down;
   * empty when there is none.
   */
  text: string;
  /** The change that holds now, if any. */
  current?: BriefChange | null;
  /** An agent's brief the operator has neither kept nor turned down. */
  pending: boolean;
  /** Every change, newest first. */
  history: Array<BriefChange>;
};

export type BriefChange = {
  event_hash: string;
  text: string;
  /** "human" (the operator), or the agent that set it. */
  by: string;
  at: string;
  /** An agent's: "kept", "turned down", or none yet. */
  decision?: string | null;
};

export type SetBriefReq = {
  /** The conversation it is for: never the caller's own. */
  stream_id: string;
  /** The whole brief. The operator may clear it with an empty one. */
  text: string;
};

export type DecideBriefReq = {
  stream_id: string;
  /** The agent's `brief.set`. */
  event_hash: string;
  /** Keep it, or turn it down (the brief before it holds again). */
  keep: boolean;
};

export type ClaudeSessions = {
  /** Newest first. */
  sessions: Array<ClaudeSession>;
};

export type ClaudeSession = {
  session_id: string;
  /** The transcript, to import by. */
  path: string;
  /** The folder it worked in, as it first recorded. */
  cwd?: string | null;
  modified: string;
  bytes: number;
  /** How many times it was compacted: a resume picks up from the last. */
  compactions: number;
  /**
   * Its newest custom title, else its newest AI title or summary, else
   * its first message, cut short.
   */
  title?: string | null;
  /** The newest title the operator gave it in Claude Code. */
  custom_title?: string | null;
  /**
   * The conversation that carries it on, while that conversation resumes
   * it.
   */
  moved_in?: string | null;
  /**
   * Moved in, and the original went on since while the thread hasn't
   * carried it on, nor is it mid-turn: importing it to the same place
   * (`suggested_destination`) brings in what is new.
   */
  can_bring_in: boolean;
  /**
   * Where it can go on: a new workspace, each workspace's orc, and for
   * each of the Bakery's folders it worked in, the most used first, the
   * conversation there, or a worker or a Baker thread made for it.
   */
  destinations: Array<Destination>;
  /**
   * The destination it fits best, never one that would refuse it: where
   * it moved in; else a session of the main checkout goes on as a new
   * workspace's orc, and one of a worktree in the conversation there, or
   * as a worker under the one workspace there is.
   */
  suggested_destination?: string | null;
};

/**
 * Where a session can go on: a conversation already there, or one that
 * moving it in opens.
 */
export type Destination = {
  /** What `POST /move-in/import` takes as `destination`. */
  key: string;
  kind: Kind4;
  /** In the operator's words. */
  label: string;
  /**
   * The folder it works in there; none for a new workspace, whose
   * worktree is made as it moves in.
   */
  worktree?: string | null;
  /** The conversation already there, which it moves into. */
  stream_id?: string | null;
  /**
   * For a new worker: the workspaces it can go under. The import's `orc`
   * picks one; with one alone, that one.
   */
  orcs: Array<Orc>;
  /**
   * The title of the session the conversation there already carries on
   * (its own title, for a session it began itself): moving in is
   * refused.
   */
  taken_by?: string | null;
};

/** What carries a session on. */
export type Kind4 = "workspace" | "worker" | "baker" | "baker_thread";

/** A workspace's orc, as a new worker's destination offers it. */
export type Orc = {
  stream_id: string;
  title: string;
};

export type ImportReq2 = {
  /** The session's transcript, as `GET /move-in/sessions` lists it. */
  transcript: string;
  /** Where it goes on: one of its destinations' keys. */
  destination: string;
  /**
   * For a new worker: the workspace it goes under, by its orc's
   * stream_id; the one workspace there is when absent.
   */
  orc?: string | null;
  /** The name of a conversation it opens; the session's title when absent. */
  title?: string | null;
};

export type Imported2 = {
  /** The conversation that resumes it. */
  stream_id: string;
  session_id: string;
  bytes: number;
  compactions: number;
};

export type MoveInHistoryQuery = {
  /** The conversation that moved in. */
  stream_id: string;
  /** The `earlier` of the page before this one; leave out for the newest. */
  before?: number | null;
  /**
   * About how many items (default 40, at most 200): a page ends on a
   * whole transcript line, so it can hold a few more.
   */
  limit?: number | null;
};

export type MoveInHistory = {
  stream_id: string;
  /**
   * The Claude Code session this conversation moved in with; none when
   * it never moved in.
   */
  session_id?: string | null;
  /** What it moved in as. */
  title?: string | null;
  /** Newest first. */
  items: Array<HistoryItem>;
  /** What this page read past without showing, by kind. */
  skipped: Array<HistorySkipped>;
  /** Pass as `before` for the page of earlier ones; none at the start. */
  earlier?: number | null;
};

export type HistoryItem = {
  /** Unique in the transcript: where its line starts, and its place in it. */
  id: string;
  kind: HistoryKind;
  /** When Claude Code recorded it. */
  at?: string | null;
  /** The words, keys and tokens left out. */
  text: string;
  /** A tool call's tool; a compaction's trigger (auto or manual). */
  name?: string | null;
  /** How long the text was before it was cut, in characters. */
  chars?: number | null;
  /** The text is cut short. */
  cut: boolean;
  /** How many keys and tokens were left out of the text. */
  left_out: number;
  /** A compaction's size before it, in tokens. */
  tokens?: number | null;
};

/** What an item is. */
export type HistoryKind = "user" | "assistant" | "tool" | "compaction" | "summary" | "unreadable";

/** What a page read past without showing. */
export type HistorySkipped = {
  /** The entry's kind in the transcript: attachment, thinking, meta... */
  kind: string;
  count: number;
};

export type PlanQuery = {
  /**
   * The Baker thread to keep, by stream_id; the one called "local
   * models" when absent.
   */
  keep?: string | null;
};

export type Plan = {
  /**
   * The Baker thread it carries over: the one named, else the one called
   * "local models".
   */
  keep?: Thread | null;
  /** The Baker threads there are, to name one when it can't tell. */
  threads: Array<Thread>;
  /**
   * The conversations it leaves behind in the old Heart, the Baker's own
   * among them.
   */
  left_behind: number;
  /** Turns running now: it never cuts one off. */
  turns_running: number;
  /** The Always-allow kinds and grants it clears. */
  always_allow: number;
  /** Worktrees the conversations it leaves behind worked in, left on disk. */
  worktrees: Array<string>;
  /** Why it can't go ahead now. */
  refused?: string | null;
};

export type Thread = {
  stream_id: string;
  title: string;
};

export type MergeThreadReq = {
  stream_id: string;
  message?: string | null;
};

export type ThreadMerged = {
  thread: string;
  branch: string;
  /** The main checkout's branch it went into. */
  into: string;
  /** The merge commit. */
  commit: string;
  /**
   * Changes the thread had not committed, which did not merge: they went
   * to the Trash with its worktree (`cleanup`), or stayed in it.
   */
  uncommitted?: string | null;
  /**
   * What became of a chef's thread, worktree and branch once its work was
   * in main; none for a workspace's merge, which leaves them as they are.
   */
  cleanup?: MergeCleanup | null;
};

/**
 * A merged chef's thread archived and its worktree and branch removed, as a
 * Delete removes them. Only the answer: `merge.done` says nothing of it.
 */
export type MergeCleanup = {
  /** The thread is archived: out of the lists, its history kept. */
  archived: boolean;
  /** Its worktree is removed. */
  worktree_removed: boolean;
  /** Its branch is deleted: its commits are in main. */
  branch_removed: boolean;
  /**
   * What it held that is in no main (changes not committed above all) was
   * kept in git first: the Trash lists it, and Put back brings it back.
   */
  kept_in_trash: boolean;
  /** Each step that didn't go, and why. What it concerns stays. */
  errors: Array<string>;
};

export type MergeWaitingList = {
  count: number;
  /** Newest first. */
  items: Array<MergeWaiting>;
};

export type MergeWaiting = {
  /** The chef's thread, the workspace, or the workspace's worker. */
  stream_id: string;
  name: string;
  project_id?: string | null;
  branch: string;
  /**
   * What it merges into: the main checkout's branch, or a worker's
   * workspace's.
   */
  base: string;
  /**
   * The conversation whose checkout it merges into: the permanent
   * Baker's (the main checkout), or a worker's orc.
   */
  into_stream_id?: string;
  /**
   * A worker's orc, which proposed it: it merges into the workspace
   * when the operator approves it.
   */
  orc?: string | null;
  /**
   * The conversation told it is ready, which reviews it and brings the
   * operator the merge review: the chef's spawner, the permanent Baker
   * when that one is archived or gone. A workspace's work has none: the
   * operator approves it in the merge review.
   */
  spawner?: string | null;
  /** Its name. */
  spawner_name?: string | null;
  /** Newest first; at most 50. */
  commits: Array<PushCommit>;
  files: Array<string>;
  adds: number;
  dels: number;
  /** What the chef said about it. */
  note?: string | null;
  asked_at: string;
};

export type PushCommit = {
  sha: string;
  subject: string;
};

export type MergeReadyReq = {
  /** The conversation asking. */
  stream_id: string;
  /**
   * A worker it opened, whose branch it proposes for its workspace; its
   * own branch, for main, when absent.
   */
  worker?: string | null;
  /** What it is, and how it was tested. */
  note?: string | null;
};

export type CreateWorkspaceReq = {
  /** What it is called; its branch is named from it. */
  title: string;
  /**
   * A catalog project's workspace, on a worktree of the project's
   * repository; the Bakery's own, beneath the Baker, when absent.
   */
  project_id?: string | null;
};

export type Workspace = {
  /** Its orc's conversation. */
  stream_id: string;
  title: string;
  worktree_path: string;
  branch: string;
};

export type DeletePlanReq = {
  target: DeleteTarget;
  /**
   * The operator's own project folders (the plan's `own`) go to the
   * Trash too.
   */
  trash_own?: boolean;
};

/** What to delete. */
export type DeleteTarget = {
  project_id: string;
  kind: "project";
} | {
  project_id: string;
  repo: string;
  kind: "repo";
} | {
  stream_id: string;
  kind: "workspace";
} | {
  stream_id: string;
  kind: "thread";
};

/** What a delete would remove, and what stays. */
export type DeletePlan = {
  target: DeleteTarget;
  /** What it is called in the lists. */
  title: string;
  /** Why it can't go ahead now; nothing goes while it says so. */
  refused?: string | null;
  /**
   * The threads and chats that go, each discarded; their history stays
   * in the Heart.
   */
  threads: Array<DeleteThread>;
  /** Worktrees the Bakery made, removed. */
  worktrees: Array<DeleteWorktree>;
  /** Branches the Bakery made for what goes, deleted. */
  branches: Array<DeleteBranch>;
  /** The unsaved work in those worktrees and branches, kept in git first. */
  saved: Array<DeleteSaved>;
  /** The threads' sandbox rooms, the node's own runtime folders, removed. */
  rooms: Array<string>;
  /**
   * A deleted project's build folder: cargo's `target` and crates, which
   * its threads' rooms shared. Build output, made again by the next
   * build: deleted outright, not sent to the Trash.
   */
  builds: Array<string>;
  containers: Array<DeleteEngineThing>;
  volumes: Array<DeleteEngineThing>;
  networks: Array<DeleteEngineThing>;
  /**
   * Container engines set up here that are off and that what goes could
   * have used: each is started, what it holds that is tied to what goes
   * removed, and stopped again.
   */
  engines: Array<Backend>;
  /**
   * Folders moved to the Trash: a chat's own, a project folder the
   * Bakery created, a project's cache, the operator's own once ticked.
   */
  trash: Array<string>;
  /**
   * The folders in `trash` with uncommitted changes, or changes that
   * can't be read: they go to the Trash with them.
   */
  trash_changes: Array<DeleteWorktree>;
  /** Every branch in a repository in `trash`: it goes to the Trash too. */
  trash_branches: Array<DeleteBranch>;
  /**
   * The project roots it lets go of (`project.root.detached`), by path:
   * a repo's, and each removed worktree's. A worktree that stays keeps
   * its root, and a deleted project keeps its own: it leaves the lists
   * whole.
   */
  roots: Array<string>;
  /**
   * The operator's own project folders (never the Bakery's own
   * repository): they stay on disk unless `trash_own`, then they are in
   * `trash`.
   */
  own: Array<string>;
  /** Folders that stay because something staying uses them, and why. */
  kept: Array<DeleteKept>;
  /**
   * Branches the Bakery made that stay because something staying uses
   * them, or their work couldn't be read, and why.
   */
  kept_branches: Array<DeleteKeptBranch>;
  /**
   * This plan's fingerprint: `POST /delete/confirm` carries it out only
   * while a fresh plan still has it.
   */
  digest: string;
};

/** A thread or chat that goes. */
export type DeleteThread = {
  stream_id: string;
  title: string;
  /** Where it worked; what becomes of the folder is in the plan's lists. */
  folder: string;
  /**
   * A pinned (`living`) one goes too: confirming the plan is the
   * confirmation its discard asks for. One pinned since is a new plan.
   */
  lifetime: Lifetime;
};

/** A worktree the Bakery made, or a folder it made for a project. */
export type DeleteWorktree = {
  /** The thread it was made for; none for a project's folder. */
  stream_id: string;
  path: string;
  /** The branch checked out in it. */
  branch: string;
  /** Its uncommitted changes, as `git status --porcelain` lists them. */
  changes: Array<StatusEntry>;
  /** Why its changes couldn't be read. */
  error?: string | null;
};

/** Uncommitted changes in a worktree (`git status --porcelain`). */
export type StatusEntry = {
  /** Two-letter porcelain code, e.g. ` M`, `M `, `??`. */
  code: string;
  path: string;
};

/** A branch. */
export type DeleteBranch = {
  name: string;
  /** The repository it is in. */
  repo: string;
};

/**
 * Unsaved work a worktree or branch that goes holds, kept in git first: the
 * Bakery's Trash lists it until the operator empties it, and Put back
 * brings it back.
 */
export type DeleteSaved = {
  repo: string;
  /** The branch, or the worktree's folder when it has none. */
  name: string;
  /** Its commits that are in no main. */
  commits: number;
  /** Its uncommitted changes. */
  changes: Array<StatusEntry>;
};

/** A container, volume or network on one of this Mac's engines. */
export type DeleteEngineThing = {
  engine: Backend;
  name: string;
  /** What ties it to what goes: its label, or its name. */
  by: string;
};

/** A folder that stays, and why. */
export type DeleteKept = {
  path: string;
  why: string;
};

/** A branch the Bakery made that stays, and why. */
export type DeleteKeptBranch = {
  name: string;
  repo: string;
  why: string;
};

export type DeleteConfirmReq = {
  target: DeleteTarget;
  /** As the plan was asked for. */
  trash_own?: boolean;
  /** The digest of the plan the operator saw. */
  digest: string;
};

/** A delete carried out, as `delete.done` records it. */
export type DeleteDone = {
  /** The plan it carried out. */
  plan: DeletePlan;
  removed: DeleteRemoved;
  /**
   * Each step that didn't go as the plan said, and why; empty when all of
   * it did. A thread whose discard failed stays, and so does what it, or
   * a project terminal that didn't end, may still use: only the roots of
   * worktrees that went and the discarded threads' own branches,
   * containers, volumes and networks go then, and a new plan lists the
   * rest.
   */
  errors: Array<string>;
};

/** What a delete removed, by the plan's own names. */
export type DeleteRemoved = {
  /** The threads discarded, by stream_id. */
  threads: Array<string>;
  worktrees: Array<string>;
  branches: Array<DeleteBranch>;
  rooms: Array<string>;
  builds: Array<string>;
  containers: Array<string>;
  volumes: Array<string>;
  networks: Array<string>;
  trash: Array<string>;
  roots: Array<string>;
};

export type TrashList = {
  /** Newest first. */
  entries: Array<TrashEntry>;
};

/** One entry of the Trash. */
export type TrashEntry = {
  /** The repository it is kept in. */
  repo: string;
  /** The branch it was, or its worktree's folder when it had none. */
  name: string;
  /** When it was kept, UTC, as its ref names it. */
  stamp: string;
  /** Its commits that are in no main. */
  commits: number;
  /** Its uncommitted changes, by path: A added, M modified, D deleted. */
  changes: Array<StatusEntry>;
};

/** An entry of the Trash, as it lists it. */
export type TrashRef = {
  repo: string;
  name: string;
  stamp: string;
};

/** A branch and worktree put back. */
export type PutBack = {
  branch: string;
  worktree: string;
};

export type EmptyReq = {
  /**
   * The entries to empty, as the Trash lists them: one, or every one it
   * shows. An entry kept since is not touched.
   */
  entries: Array<TrashRef>;
  /**
   * Must be true: emptying can't be undone, and the operator was asked
   * first.
   */
  confirm?: boolean;
};

export type Emptied = {
  /** Entries that went. */
  emptied: number;
  /** Entries that couldn't, each with why. */
  refused: Array<string>;
};

export type SendReq = {
  /** The conversation it goes to, by stream_id, or `baker` for the Baker. */
  to: string;
  text: string;
  /** The message this answers, by its id. */
  reply_to?: string | null;
  /**
   * Send it as a note: it shows in their conversation without starting a
   * turn, and their next turn reads it. A message (the default) starts a
   * turn there, so send one only when they must act on it.
   */
  note?: boolean;
};

export type Sent = {
  id: string;
  to: string;
  /** A message's place in the receiver's queue; a note waits in none. */
  queued?: string | null;
};

export type SavedQuery = {
  /** One conversation's; every conversation's when absent. */
  stream_id?: string | null;
};

export type SavedList = {
  /** Newest first. */
  items: Array<SavedReply>;
};

export type SavedReply = {
  id: string;
  stream_id: string;
  /** The reply's `agent.turn.done` event. */
  event_hash: string;
  /** The reply as it was when saved: its summary, what its Copy copies. */
  text: string;
  saved_at: string;
  /**
   * The checklist items ticked so far, numbered from 0 in the text's
   * order (see [`checklist`]).
   */
  done: Array<number>;
};

export type SaveReq = {
  stream_id: string;
  /** The reply's `agent.turn.done` event. */
  event_hash: string;
};

export type SavedId = {
  id: string;
};

export type Unsaved = {
  id: string;
};

export type TickReq = {
  id: string;
  /** The checklist item, numbered from 0. */
  item: number;
  done: boolean;
};

export type MemoryImportReq = {
  /** Whose memory: a Baker thread's goes to the one Baker memory. */
  stream_id: string;
  /**
   * A folder of notes in Claude Code's memory form: one Markdown file a
   * note, with `name`, `description` and `metadata.type` in its front
   * matter; MEMORY.md, its index, is left out.
   */
  folder: string;
};

export type MemoryImported = {
  /** The notes written, new or changed, by name. */
  imported: Array<string>;
  /** The notes already the same in the Bakery. */
  unchanged: Array<string>;
  /** Files not imported, each with why. */
  skipped: Array<string>;
};

/**
 * Context composition for one stream.
 *
 * NODE_API §4 asks for a token window (`total/used/system/files/transcript/
 * memory`). The node has no tokenizer and records no usage, so those numbers
 * do not exist and are **not** reported — a plausible token count is worse
 * than none, because it would be trusted. What is measured instead is
 * characters of the actual assembled prompt pieces, named as such.
 *
 * The context cap (`total`) is a property of whichever model is configured,
 * which the node does not know either, so there is no "used / total" bar
 * here. That field returns when usage recording lands.
 */
export type ContextView = {
  stream_id: string;
  /** Real character counts. Not tokens — see the type comment. */
  prompt_chars: PromptChars;
  limits: ContextLimits;
  files_in_scope: Array<FileScope>;
  /**
   * Count of refusals before the cap — so a client can render
   * "showing 50 of 213" instead of mistaking the capped list for complete.
   */
  refusals_total: number;
  /**
   * The jail refusing things, newest last (at most `REFUSALS_CAP`).
   * The highest-value part of this view: it is the jail proving it works.
   */
  refusals: Array<Refusal>;
};

export type PromptChars = {
  /** The tool-loop system prompt, verbatim length. */
  system: number;
  /** Hot-memory block as it would be injected this turn. */
  memory: number;
  total: number;
};

export type ContextLimits = {
  max_tool_steps?: number | null;
  hot_memory_scan_events: number;
  hot_memory_max_lines: number;
};

export type FileScope = {
  glob: string;
  /**
   * `read+propose`: agent tools read inside the jail, and a write is only
   * ever a proposal. Nothing the agent does writes directly.
   */
  mode: string;
};

export type Refusal = {
  at: string;
  /** Tool that was refused, or `edit` for a refused proposal path. */
  tool: string;
  /** Path the agent reached for, when the call carried one. */
  path?: string | null;
  reason: string;
};

export type OpenWorktreeReq = {
  /** Path to an existing git repository. */
  repo: string;
  /** Worktree name (also the branch suffix and stream key). Must be unique. */
  name: string;
};

export type ProvisionReq = {
  stream_id: string;
};

export type ProvisionResp = {
  digest: string;
  root: string;
  cache_hit: boolean;
  rustc?: string | null;
};

export type AttachReq = {
  stream_id: string;
  argv: Array<string>;
};

export type ExecResp = {
  stdout: string;
  stderr: string;
  exit_code: number;
};

export type EnvQuery = {
  stream_id: string;
};

/**
 * The room capability plane (07b/07b-B): `thin_ready` is true for any
 * resolvable stream (thin admission has no prerequisites), and
 * `project_tools` reports the *optional* pinned-toolchain capability
 * honestly, without triggering a real provision from a GET:
 *
 * - `not_configured` — no `mise.toml` at all. Not an error.
 * - `invalid` — a `mise.toml` exists but does not parse (malformed TOML, or
 * a tool value that isn't a version string). Distinct from
 * `not_configured`: there IS a manifest, it's just broken.
 * - `unsupported` — parses fine but pins a tool this backend cannot
 * provision (anything but `rust`).
 * - `configured` — pins only supported tools but is not yet cached; using
 * it would pay a real (possibly multi-minute) toolchain clone. `resolve_room`
 * (exec/PTY) never does that as a side effect — only `/env/provision` does.
 * - `ready` — cached and warm; `resolve_room` will actually attach it.
 */
export type EnvStatusResp = {
  thin_ready: boolean;
  project_tools: string;
  reason?: string | null;
};

export type CreateReq = {
  stream_id: string;
  name?: string | null;
  mode: string;
  /**
   * Set only for a system account login pane (07d-A3D1) — `stream_id`
   * must then be [`terminals::SYSTEM_ACCOUNTS_STREAM_ID`], and `mode` is
   * ignored (always resolved as an unsandboxed host process, same as
   * `mode: "host"`, since the login CLI needs the human's real
   * credentials/network, not a Project worktree or sandbox).
   */
  command?: LoginCommandShape | null;
};

/** The shape of `bakery_agent::cli_tools::LoginCommand`. */
export type LoginCommandShape = "codex-login" | "cursor-login" | "claude-native-login";

export type ListQuery2 = {
  stream_id: string;
};

export type RenameReq = {
  stream_id: string;
  terminal_id: string;
  name: string;
};

export type ReadQuery = {
  stream_id: string;
  terminal_id: string;
  /** The last this many bytes it printed: 8000 by default, 64000 at most. */
  max_bytes?: number | null;
};

export type TerminalText = {
  text: string;
  live: boolean;
  /** Something is running in it now. */
  running: boolean;
};

export type CloseReq = {
  stream_id: string;
  terminal_id: string;
};

export type TicketReq = {
  stream_id: string;
  terminal_id: string;
};

export type TicketResp = {
  ticket: string;
  expires_in_secs: number;
};

export type JobListQuery = {
  /** Only this conversation's jobs. */
  stream_id?: string | null;
};

export type JobView = {
  id: string;
  stream_id: string;
  command: string;
  cwd: string;
  /** The studio terminal showing it. */
  terminal_id: string;
  started_at: string;
  /** running, exited or stopped. */
  state: string;
  exit_code?: number | null;
};

export type JobStartReq = {
  /**
   * The conversation whose sandbox runs it. The conversation that starts
   * it hears when it ends: yours, when you call this.
   */
  stream_id: string;
  /** A shell command line, run with /bin/sh -c. */
  command: string;
  /**
   * Where it starts, inside that conversation's workspace (default: its
   * root).
   */
  cwd?: string | null;
};

export type JobOutputQuery = {
  id: string;
  /** The offset to read from: 0, or the `next` of the last call. */
  since?: number | null;
};

export type JobOutput = {
  id: string;
  stream_id: string;
  command: string;
  cwd: string;
  /** The studio terminal showing it. */
  terminal_id: string;
  started_at: string;
  /** running, exited or stopped. */
  state: string;
  exit_code?: number | null;
  output: string;
  /** Pass as `since` to read on from here. */
  next: number;
  /** Output before `since` that the job no longer keeps. */
  truncated: boolean;
};

export type JobStopReq = {
  id: string;
};

export type GitQuery = {
  stream_id: string;
};

export type GitStatus = {
  /** `None` on a detached HEAD. */
  branch?: string | null;
  upstream?: string | null;
  ahead: number;
  behind: number;
  clean: boolean;
  files: Array<FileStatus>;
};

export type FileStatus = {
  path: string;
  /** A rename's old path. */
  from?: string | null;
  /**
   * What is staged for it: modified, added, deleted, renamed, copied,
   * type changed.
   */
  staged?: string | null;
  /** What is changed and not staged. */
  unstaged?: string | null;
  untracked: boolean;
  conflicted: boolean;
};

export type Branch = {
  name: string;
  current: boolean;
  upstream?: string | null;
  /** e.g. "ahead 2, behind 1"; empty when even with its upstream. */
  track: string;
  commit: string;
  date: string;
  subject: string;
};

export type LogQuery = {
  stream_id: string;
  /** How many commits (default 30, at most 200). */
  limit?: number | null;
  /** A branch or revision (default: the current branch). */
  rev?: string | null;
};

export type Commit = {
  sha: string;
  short: string;
  author: string;
  date: string;
  subject: string;
};

export type DiffQuery = {
  stream_id: string;
  /** One file only. */
  path?: string | null;
  /** What is staged, instead of what is not. */
  staged?: boolean;
  /** One commit's changes instead. */
  commit?: string | null;
  /**
   * Everything this checkout's branch has committed since it left `base`
   * (`git diff <base>...HEAD`), e.g. a whole agent branch against main.
   */
  base?: string | null;
  /**
   * With `base`: what is not committed yet too, so all of the branch's
   * changes since it left `base` show together
   * (`git diff $(git merge-base <base> HEAD)`).
   */
  worktree?: boolean;
};

export type Diff = {
  diff: string;
  truncated: boolean;
  /**
   * It shows lines of a secret file (`machine_access::is_secret`): no
   * desktop screenshot while the app shows it.
   */
  secret: boolean;
};

export type StageReq = {
  stream_id: string;
  paths?: Array<string>;
  /** Every change, new files included. */
  all?: boolean;
  /** Take these paths out of the staged set instead. */
  unstage?: boolean;
};

export type CommitReq = {
  stream_id: string;
  message: string;
};

export type BranchReq = {
  stream_id: string;
  name: string;
  /** Where it starts (default: the current commit). */
  from?: string | null;
  /** Switch to it (default true). */
  switch?: boolean;
};

export type SwitchReq = {
  stream_id: string;
  branch: string;
};

export type FetchReq = {
  stream_id: string;
  /** Default: origin. */
  remote?: string | null;
};

export type Updated = {
  /** What it merged in: the branch this thread forked from. */
  base: string;
  /** It merged cleanly, or there was nothing to bring in. */
  merged: boolean;
  /**
   * Files in conflict. The merge is left in progress for the operator or
   * an agent to resolve, then commit (or `git merge --abort`).
   */
  conflicts: Array<string>;
  output: string;
};

export type RebaseReq = {
  stream_id: string;
  /** The branch or revision to rebase onto. */
  onto: string;
  /** Why a rebase and not a merge (`git_update`): recorded in History. */
  reason: string;
};

export type PushReq = {
  stream_id: string;
  /** Default: the current branch. */
  branch?: string | null;
  /**
   * The commit to send, on that branch: the one its card was approved
   * for. Default: the branch's newest.
   */
  commit?: string | null;
};

export type Done = {
  branch: string;
  output: string;
};

/**
 * A change a room sent to Google's APIs with the operator's own sign-in,
 * as its card shows it.
 */
export type GoogleChange = {
  stream_id: string;
  /** `POST`, `PUT`, `PATCH` or `DELETE`. */
  method: string;
  /** Where it goes, as the room sent it: stand-ins, never a real token. */
  url: string;
  /** What it sends: its fields' names, never their values. */
  sends: string;
};

export type PrReq = {
  stream_id: string;
  title: string;
  body?: string | null;
  /** The branch it merges into (default: the repository's default). */
  base?: string | null;
};

export type MergeReq = {
  stream_id: string;
  /** The branch to merge into the current one. */
  branch: string;
  message?: string | null;
};

export type MergePreviewQuery = {
  stream_id: string;
  /** The branch this one would merge into. */
  base: string;
  /**
   * With the diff too (cut at 32 KiB). Without it, the files and their
   * lines, and the conflicts.
   */
  diff?: boolean;
};

export type MergePreview = {
  /** It merges without a conflict. */
  clean: boolean;
  /** The files that would conflict; their conflict markers are in `diff`. */
  conflicts: Array<string>;
  /**
   * Each file the merge would change on base, with its lines: the first
   * 200.
   */
  files: Array<ChangedFile>;
  /** How many files it would change, all of them. */
  files_total: number;
  /** Lines it would add and remove on base, over every file. */
  added: number;
  removed: number;
  /**
   * What the merge would change on base: base against the merged result,
   * cut at 32 KiB. Empty unless asked for (`diff`).
   */
  diff: string;
  /** The diff was cut. */
  truncated: boolean;
  /**
   * It shows lines of a secret file: no desktop screenshot meanwhile.
   * Without the diff, one of its files is a secret file.
   */
  secret: boolean;
};

/** A file a merge would change, as `git diff --numstat` counts it. */
export type ChangedFile = {
  path: string;
  /** A rename's old path. */
  from?: string | null;
  added: number;
  removed: number;
  /** No lines to count. */
  binary: boolean;
};

export type MergeChainQuery = {
  stream_id: string;
  /** The branch they would all merge into. */
  base: string;
  /** The branches to merge into it, in order. */
  branches: Array<string>;
  /**
   * With the diff too (cut at 32 KiB). Without it, the files and their
   * lines, and each step's conflicts.
   */
  diff?: boolean;
};

export type MergeChain = {
  /** One per branch, in order. */
  steps: Array<ChainStep>;
  /**
   * Each file every merge together would change on base, with its
   * lines: the first 200.
   */
  files: Array<ChangedFile>;
  /** How many files they would change, all of them. */
  files_total: number;
  /** Lines they would add and remove on base, over every file. */
  added: number;
  removed: number;
  /**
   * base against the result of every merge, conflict markers included,
   * cut at 32 KiB. Empty unless asked for (`diff`).
   */
  diff: string;
  /** The diff was cut. */
  truncated: boolean;
  /**
   * It shows lines of a secret file: no desktop screenshot meanwhile.
   * Without the diff, one of its files is a secret file.
   */
  secret: boolean;
};

export type ChainStep = {
  branch: string;
  /** It merges into base and every branch before it without a conflict. */
  clean: boolean;
  conflicts: Array<string>;
};

export type PrChecksQuery = {
  stream_id: string;
};

export type PrChecks = {
  branch: string;
  checks: Array<PrCheck>;
};

export type PrCheck = {
  name: string;
  /** GitHub's state: SUCCESS, FAILURE, PENDING, SKIPPED… */
  state: string;
  /** pass, fail, pending, skipping or cancel. */
  bucket?: string;
  workflow?: string;
  link?: string;
};

export type ReposQuery = {
  /**
   * Only this owner's: the operator or one of their organizations. All of
   * them by default.
   */
  owner?: string | null;
};

export type Repos = {
  /** Most recently updated first. */
  repos: Array<Repo>;
  /** Owners GitHub couldn't list (an organization's SSO, say); the rest did. */
  problems: Array<string>;
};

export type Repo = {
  /** owner/name. */
  repository: string;
  owner: string;
  name: string;
  /** public, private or internal. */
  visibility: string;
  default_branch?: string | null;
  updated_at: string;
  description: string;
  archived: boolean;
  fork: boolean;
  url: string;
  /**
   * Where it is cloned on this Mac: a Project's repository, or the
   * default clone folder (~/Dev/code/<name>).
   */
  cloned_at?: string | null;
};

export type CloneReq = {
  /** owner/name. */
  repository: string;
  /**
   * A new folder in your home, as a full path or ~/…; ~/Dev/code/<name>
   * by default.
   */
  into?: string | null;
  /**
   * An existing project the clone joins as one of its roots, instead of
   * becoming a project of its own.
   */
  project_id?: string | null;
};

export type Cloned = {
  path: string;
  project: ProjectView;
  socket: ActiveContext;
};

export type IssuesQuery = {
  /** owner/name. */
  repository: string;
  /** open (the default), closed or all. */
  state?: string | null;
  /** At most this many, most recent first: 50 by default, 500 at most. */
  limit?: number | null;
};

export type Issue = {
  number: number;
  title: string;
  /** OPEN or CLOSED. */
  state: string;
  author: string;
  labels: Array<string>;
  assignees: Array<string>;
  updated_at: string;
  url: string;
};

export type IssueQuery = {
  /** owner/name. */
  repository: string;
  number: number;
};

export type IssueView = {
  number: number;
  title: string;
  state: string;
  author: string;
  body: string;
  labels: Array<string>;
  assignees: Array<string>;
  created_at: string;
  updated_at: string;
  url: string;
  /** Oldest first. */
  comments: Array<IssueComment>;
};

export type IssueComment = {
  author: string;
  body: string;
  created_at: string;
  url: string;
};

export type IssueCreateReq = {
  /** owner/name. */
  repository: string;
  title: string;
  body?: string;
  labels?: Array<string>;
};

export type Published = {
  /** Where it now is on GitHub. */
  url: string;
};

export type IssueCommentReq = {
  /** owner/name. */
  repository: string;
  number: number;
  body: string;
};

export type GithubScope = {
  /** The operator's GitHub login. */
  me: string;
  /**
   * The operator, their organizations, then any other owner added; each
   * on unless turned off.
   */
  owners: Array<Owner>;
  /** Repositories turned on or off whatever their owner is. */
  repos: Array<RepoOn>;
  /** What GitHub couldn't answer (the operator's organizations, say). */
  problems: Array<string>;
};

export type Owner = {
  login: string;
  /** user or org. */
  kind: string;
  on: boolean;
};

export type RepoOn = {
  /** owner/name. */
  repository: string;
  on: boolean;
};

export type ScopeReq = {
  /** Owners to turn on or off; a login not listed yet is added. */
  owners?: Array<OwnerOn>;
  /** Repositories to turn on or off whatever their owner is. */
  repos?: Array<RepoOn>;
};

export type OwnerOn = {
  login: string;
  on: boolean;
};

export type Prs = {
  me: string;
  /** When GitHub gave this answer. */
  fetched_at: string;
  /** Most recently updated first. */
  prs: Array<GithubPr>;
  /** Owners or repositories GitHub couldn't answer for; the rest did. */
  problems: Array<string>;
};

export type GithubPr = {
  /** owner/name, as GitHub writes it. */
  repository: string;
  number: number;
  title: string;
  url: string;
  author: string;
  author_is_bot: boolean;
  /** The branch it would merge. */
  branch: string;
  /** The branch it would merge into. */
  base: string;
  draft: boolean;
  created_at: string;
  updated_at: string;
  /** APPROVED, CHANGES_REQUESTED or REVIEW_REQUIRED. */
  review_decision?: string | null;
  /** A review is asked of the operator, directly or through a team. */
  requested_from_me: boolean;
  /** The head commit's checks, each one's latest attempt. */
  checks: CheckCounts;
  /** MERGEABLE, CONFLICTING or UNKNOWN. */
  mergeable: string;
  /** CLEAN, BLOCKED, BEHIND, DIRTY, UNSTABLE, HAS_HOOKS, DRAFT or UNKNOWN. */
  merge_state: string;
  additions: number;
  deletions: number;
  changed_files: number;
  /**
   * The repository deletes a pull request's branch once it is merged
   * (GitHub's "Automatically delete head branches"): where the merge's
   * delete-branch choice starts.
   */
  delete_branch_on_merge: boolean;
  /** Its conversation's comments. */
  comments: number;
  /**
   * The thread that opened it, else a live worktree on its branch in a
   * clone of its repository; never a checkout opened in place.
   */
  thread_stream_id?: string | null;
  /** The project its repository is a root of. */
  project_id?: string | null;
};

export type CheckCounts = {
  pass: number;
  /** Cancelled ones too. */
  fail: number;
  pending: number;
  skipped: number;
};

export type PrQuery = {
  /** owner/name. */
  repository: string;
  number: number;
};

export type PrView = {
  pr: GithubPr;
  body: string;
  files: Array<PrFile>;
  /** Empty and truncated when GitHub won't give a diff this big. */
  diff: string;
  diff_truncated: boolean;
  /**
   * The diff shows a credential-shaped file: the app keeps it out of
   * screenshots.
   */
  secret: boolean;
  checks: Array<GithubCheck>;
  /** The last 100, oldest first. */
  reviews: Array<Review>;
  threads: Array<ReviewThread>;
  /** The conversation's last 100, oldest first. */
  comments: Array<Comment>;
  /** The last 100, oldest first. */
  commits: Array<PrCommit>;
};

export type PrFile = {
  path: string;
  /** Where a renamed file was. */
  previous_path?: string | null;
  /** added, modified, removed or renamed. */
  status: string;
  additions: number;
  deletions: number;
};

export type GithubCheck = {
  name: string;
  /** GitHub's state: SUCCESS, FAILURE, IN_PROGRESS, PENDING, SKIPPED… */
  state: string;
  /** pass, fail, pending, skipping or cancel, as gh pr checks has them. */
  bucket: string;
  workflow: string;
  link: string;
  /**
   * The Actions run and job behind it; none for a commit status or
   * another app's check.
   */
  run_id?: number | null;
  job_id?: number | null;
};

export type Review = {
  id: number;
  author: string;
  /** APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED or PENDING. */
  state: string;
  body: string;
  submitted_at: string;
};

export type ReviewThread = {
  /** What resolving it names as `thread_id`. */
  id: string;
  path: string;
  /** None once the lines it was on are gone. */
  line?: number | null;
  start_line?: number | null;
  /** LEFT (the old side) or RIGHT (the new). */
  side: string;
  resolved: boolean;
  outdated: boolean;
  comments: Array<Comment>;
};

export type Comment = {
  /** What a reply in a review thread names as `in_reply_to`. */
  id: number;
  author: string;
  body: string;
  created_at: string;
  url: string;
};

export type PrCommit = {
  sha: string;
  subject: string;
  /** Their GitHub login, else the name on the commit. */
  author: string;
  committed_at: string;
};

export type ReviewReq2 = {
  /** owner/name. */
  repository: string;
  number: number;
  /** COMMENT, APPROVE or REQUEST_CHANGES. */
  event: string;
  body?: string;
  comments?: Array<LineComment>;
};

export type LineComment = {
  path: string;
  /** The comment's last line; with `start_line`, it spans the lines between. */
  line: number;
  start_line?: number | null;
  /** LEFT (the old side) or RIGHT (the new). */
  side: string;
  body: string;
};

export type Reviewed2 = {
  review_id: number;
  url: string;
};

export type CommentReq = {
  /** owner/name. */
  repository: string;
  number: number;
  body: string;
  /**
   * A review thread comment's id, to reply in its thread; the
   * conversation otherwise.
   */
  in_reply_to?: number | null;
};

export type Commented = {
  comment_id: number;
  url: string;
};

export type PrMergeReq = {
  /** owner/name. */
  repository: string;
  number: number;
  /** merge or squash. */
  method: string;
  /**
   * Delete its branch once merged; false keeps it, even in a repository
   * that deletes merged branches itself (it is restored at the pull
   * request's head, as GitHub's "Restore branch" does). A branch in
   * another repository (a fork) or the default branch is never touched.
   */
  delete_branch?: boolean;
};

export type Merged = {
  merged: boolean;
  /** The commit the merge made. */
  sha: string;
  /**
   * Its branch is gone once the merge is done: what is true, whoever
   * deleted it.
   */
  branch_deleted: boolean;
};

export type ReadyReq = {
  /** owner/name. */
  repository: string;
  number: number;
};

export type Ready = {
  draft: boolean;
};

export type ResolveReq = {
  /** owner/name. */
  repository: string;
  /** A review thread's `id`. */
  thread_id: string;
  /** False opens it again. */
  resolved: boolean;
};

export type Resolved = {
  resolved: boolean;
};

export type Runs = {
  fetched_at: string;
  /** The latest of each workflow on each branch, newest first. */
  runs: Array<GithubRun>;
  /** Repositories GitHub couldn't answer for; the rest did. */
  problems: Array<string>;
};

export type GithubRun = {
  /** owner/name. */
  repository: string;
  run_id: number;
  branch: string;
  workflow: string;
  title: string;
  /** queued, in_progress or completed. */
  status: string;
  /** success, failure, cancelled…; empty until it completes. */
  conclusion: string;
  /** What started it: push, pull_request, schedule… */
  event: string;
  created_at: string;
  updated_at: string;
  url: string;
  /** A live worktree on its branch in a clone of its repository. */
  thread_stream_id?: string | null;
  project_id?: string | null;
};

export type RunQuery = {
  /** owner/name. */
  repository: string;
  run_id: number;
};

export type RunView = {
  run: GithubRun;
  jobs: Array<Job>;
};

export type Job = {
  job_id: number;
  name: string;
  status: string;
  conclusion: string;
  started_at: string;
  completed_at: string;
  steps: Array<JobStep>;
};

export type JobStep = {
  number: number;
  name: string;
  status: string;
  conclusion: string;
};

export type JobLogQuery = {
  /** owner/name. */
  repository: string;
  job_id: number;
};

export type RunLog = {
  job_id: number;
  steps: Array<StepLog>;
  /** A step's log is longer than the 64 KiB it keeps (its end). */
  truncated: boolean;
};

export type StepLog = {
  /** 0 for lines gh couldn't place in a step. */
  number: number;
  name: string;
  conclusion: string;
  log: string;
};

export type RerunReq = {
  /** owner/name. */
  repository: string;
  run_id: number;
  /** Only the jobs that failed, and what depends on them. */
  failed_only?: boolean;
};

export type Queued = {
  queued: boolean;
};

export type GrantsQuery = {
  stream_id: string;
};

export type PushGrant = {
  grant_id: string;
  /**
   * push: the conversation pushes this branch without asking, never
   * forced. google: its rooms change this Google Cloud project without
   * asking, never deleting.
   */
  kind: string;
  /**
   * owner/name on GitHub, else the remote's address; for google, the
   * Google Cloud project.
   */
  repository: string;
  /** Empty for google. */
  branch: string;
  /** Always the operator. */
  given_by: string;
  at: string;
};

export type GiveReq = {
  stream_id: string;
  /** push, the one kind so far. */
  kind: string;
  /** Default: the conversation's current branch. */
  branch?: string | null;
};

export type WaitingList = {
  count: number;
  /** Newest first. */
  items: Array<Waiting>;
};

export type Waiting = {
  stream_id: string;
  /** The conversation's name. */
  name: string;
  project_id?: string | null;
  /** The card, as decided through /agent/pipe/gate/decide. */
  card: string;
  /**
   * git_push, git_pr, github_issue_create, github_issue_comment or
   * google_change (a room's change to Google Cloud, held at its door).
   */
  tool: string;
  /** Untyped JSON: no schema on the node describes it yet. */
  args: unknown;
  asked_at: string;
  /** What a push sends, as its card shows it, less the diff text. */
  push?: PushSummary | null;
  /**
   * The card's offer to let the conversation push that branch, or change
   * that Google Cloud project, from now on, taken by deciding the card
   * AllowAlways.
   */
  grant?: GrantOffer | null;
};

/** A push card's preview without its diff (`routes::git::PushPreview`). */
export type PushSummary = {
  branch: string;
  /** The commit it sends. */
  head: string;
  /** The remote's branch as last fetched; none when it is new there. */
  remote_head?: string | null;
  base: string;
  /** The commits it adds, newest first; at most 50. */
  commits: Array<PushCommit>;
  diff_truncated: boolean;
  /** The diff shows a credential-shaped file. */
  secret: boolean;
};

/**
 * A card's offer (`grants::offer`): this branch, or this Google Cloud
 * project (`repository`, with no branch), from now on.
 */
export type GrantOffer = {
  /** push or google */
  kind: string;
  repository: string;
  branch: string;
};

export type HistoryQuery = {
  /** At most this many, newest first: 100 by default, 1000 at most. */
  limit?: number | null;
};

export type RemoteAction = {
  at: string;
  /**
   * Who did it: "human" (the operator), or the agent whose call the
   * operator approved or granted.
   */
  by: string;
  /**
   * What was done: `{what, repository, …, stream_id?}`.
   * Untyped JSON: no schema on the node describes it yet.
   */
  line: unknown;
};

export type EndReq = {
  stream_id: string;
  grant_id: string;
};

export type ProvenanceQuery = {
  stream_id: string;
  /**
   * The branch it left: the diff is base...HEAD, what this branch
   * committed since, never uncommitted work.
   */
  base: string;
  /** One file only. */
  path?: string | null;
};

export type Provenance = {
  hunks: Array<HunkSource>;
};

export type HunkSource = {
  /** The file, by its new path. */
  file: string;
  /** From the hunk's @@ … +new_start,new_lines @@ header. */
  new_start: number;
  new_lines: number;
  /** Its added lines, not counting blank ones. */
  added: number;
  /** The recorded write that introduced most of them; null when none did. */
  source?: Source2 | null;
};

export type Source2 = {
  stream_id: string;
  turn_id: string;
  call_id?: string | null;
  /**
   * agent.file.changed (an edit Bakery reviewed), agent.acp.tool_call (a
   * native tool's edit) or operator.file.edited (the operator's own save).
   */
  family: string;
  event_hash: string;
  /** When it was written. */
  at: string;
  actor: string;
  /** The turn's first message, at most 500 characters. */
  instruction?: string | null;
  /**
   * How many of the hunk's added lines this write introduced; fewer than
   * `added` means the hunk mixes writes (or hand edits).
   */
  lines_matched: number;
};

/**
 * layout.ts's `Layout`, version 1: workspaces, each a strip of columns,
 * each column a stack of panels, plus the Baker's own workspace and the
 * sidebar. A layout saved with the old Baker `dock` still loads.
 */
export type Layout = {
  /** Always 1. */
  version: number;
  /** Which side the window shows; bakery when unset. */
  mode?: Mode | null;
  /** The Bakery workspace shown in bakery mode. */
  active?: string | null;
  workspaces: Array<Workspace2>;
  /** The Baker's own workspace (id "baker"): its threads and what it opens. */
  baker?: Workspace2 | null;
  /**
   * Projects on the Baker's side (the Bakery's own repo): shown in baker
   * mode, with no Bakery workspace of their own.
   */
  bakerProjects?: Array<string>;
  sidebar: Sidebar;
  /** The bottom panel: the focused agent's terminals. */
  bottom?: Bottom | null;
};

export type Mode = "bakery" | "baker";

export type Workspace2 = {
  id: string;
  name: string;
  /** Set for the one-per-project workspaces; null for one made by hand. */
  projectId?: string | null;
  columns: Array<Column>;
  /** The focused panel. */
  focus?: string | null;
  /** How it lays out its columns; screen when unset. */
  view?: View2 | null;
  /** Panels locked open: they show no close button and don't close. */
  locked?: Array<string>;
};

export type Column = {
  id: string;
  width: number;
  /**
   * Panel ids, top to bottom: a conversation's stream id, or "@review",
   * "@containers", "@code:<stream>", "@preview:<stream>".
   */
  panels: Array<string>;
  /** The tab it shows in the screen view. */
  active?: string | null;
};

export type View2 = "screen" | "strip";

export type Sidebar = {
  open: boolean;
  width: number;
  /** What it lists; agents when unset. */
  mode?: SidebarMode | null;
};

export type SidebarMode = "agents" | "worktrees";

export type Bottom = {
  open: boolean;
  height: number;
};

export type SaveLayoutReq = {
  layout: Layout;
};

export type SaveAppearanceReq = {
  /**
   * The app's AppearanceState (version 1: selected, schemes), stored as
   * given.
   * Untyped JSON: no schema on the node describes it yet.
   */
  appearance: unknown;
};

export type BundleQuery = {
  stream_id: string;
};

export type Bundle = {
  /** What the app checks it against. */
  hash: string;
  files: number;
  bytes: number;
  /** The checkout it was built in: its branch (None when detached) and commit. */
  branch?: string | null;
  commit: string;
  /** bakery-client has uncommitted changes: the build may hold work no commit has. */
  client_dirty: boolean;
  /** The bundle the deployed app shows now. */
  current?: string | null;
};

export type InstallReq2 = {
  stream_id: string;
  /** The hash ui_bundle answered: the build the operator approves. */
  hash: string;
};

export type Installed = {
  hash: string;
  /** The bundle it replaced, which ui_bundle_rollback puts back. */
  previous?: string | null;
};

export type RolledBack = {
  /** The bundle the app shows now. */
  hash: string;
  /** The one it took off. */
  replaced?: string | null;
};

export type PairReq = {
  /** The code Bakery shows (Settings, Machines). */
  code: string;
  /** What to call this device, e.g. "Sam's iPhone". */
  name: string;
  kind: DeviceKind;
};

export type DeviceKind = "phone" | "node";

export type PairResp = {
  device_id: string;
  /**
   * The device's own token: keep it in the device's secure store. The
   * node never shows it again.
   */
  token: string;
};

export type Devices = {
  remote: RemoteStatus;
  devices: Array<DeviceView>;
};

export type RemoteStatus = {
  /** The operator let this node be reached over the tailnet. */
  enabled: boolean;
  /** Where phones and machines reach it, while it listens. */
  url?: string | null;
  /** Why it isn't listening, when it should be: what to turn on. */
  problem?: string | null;
};

export type DeviceView = {
  device_id: string;
  kind: DeviceKind;
  name: string;
  paired_at: string;
  /** Its last call since this node started. */
  last_seen?: string | null;
};

export type PairCode = {
  /** Type it into the device, or scan it from the QR code. */
  code: string;
  expires_in_secs: number;
  /** Where the device reaches this node. */
  url: string;
};

export type RevokeReq = {
  device_id: string;
};

export type RemoteReq = {
  /** Reachable from the operator's other devices over Tailscale. */
  enabled: boolean;
};

export type OutsideWindow = {
  /** When the current token was made; none while there is none. */
  created_at?: string | null;
  /** Its last call since this node started. */
  last_used?: string | null;
  /** Where an outside session reaches it. */
  url: string;
  /** What it may call. */
  tools: Array<string>;
};

export type Created = {
  /** The token, shown this once: the node keeps only its hash. */
  token: string;
  window: OutsideWindow;
};

export type PreviewReq2 = {
  /**
   * The conversation whose preview it is: an agent's own, which is also
   * the default.
   */
  stream_id?: string;
  /** Which of its open previews; the one opened last by default. */
  view_id?: string | null;
};

export type PreviewSnapshot = {
  url: string;
  title: string;
  /** The page's visible text, at most 20,000 characters. */
  text: string;
};

export type PreviewNavigateReq = {
  stream_id?: string;
  view_id?: string | null;
  /** An http(s) URL. */
  url: string;
};

export type PreviewPage = {
  url: string;
  title: string;
};

export type PreviewClickReq = {
  stream_id?: string;
  view_id?: string | null;
  /** A CSS selector; the first element it matches is clicked. */
  selector: string;
};

export type ScreenshotReq = {
  /**
   * The conversation asking, where the capture is noted: an agent's own,
   * which is also the default.
   */
  stream_id?: string;
};

export type Screenshot = {
  width: number;
  height: number;
  /**
   * The PNG, base64. An agent's tool result carries only the size: its
   * model gets the image itself beside the result.
   */
  png: string;
};

export type PreviewEvalReq = {
  stream_id?: string;
  view_id?: string | null;
  /** A JavaScript expression, awaited if it is a promise. */
  expression: string;
};

export type PreviewValue = {
  /** The value as JSON, at most 20,000 characters, or "undefined". */
  value: string;
};

export type GhosttyAvailable = {
  available: boolean;
};

export type GhosttyOpenReq = {
  stream_id: string;
};

export type GhosttyOpenResp = {
  opened: boolean;
};

export type SeatAssignReq = {
  stream_id: string;
  seat: Seat;
  model_id: string;
  /**
   * Claude native's own setting — "subscription" | "console" |
   * "openrouter"; refused on any other model, and on an unknown id.
   * Left out, a native seat keeps whichever it already had (default
   * subscription).
   */
  provider?: string | null;
  /** Why, for a switch you decide: recorded with the model the seat had. */
  reason?: string | null;
  /**
   * The hash of the event that caused the switch. A model.quota_exhausted
   * hands the conversation over to the new model when its last turn
   * stopped on that quota.
   */
  because?: string | null;
};

/**
 * A role on a stream (STEERING.md § Models: "seats ≠ models" — the seat is
 * who/what a turn is answering as; the model id in [`SeatAssignment`] is
 * which client answers it, and the two vary independently). At least these
 * three exist per STEERING's seats+A primitive; more may be added later the
 * same way these were — a closed enum, not an open string, so a typo'd seat
 * name fails to compile rather than silently minting a seat nobody assigns.
 */
export type Seat = "orchestrator" | "worker" | "baker";

export type ChildToolGateDecideReq = {
  stream_id: string;
  event_hash: string;
  decision: ChildToolGateDecisionKind;
};

export type ChildToolGateDecisionKind = "allow_once" | "allow_always" | "deny";

export type LifetimeReq = {
  stream_id: string;
  lifetime: Lifetime;
};

export type LifetimeResp = {
  stream_id: string;
  lifetime: Lifetime;
};

export type DiscardReq = {
  stream_id: string;
  /**
   * Required (and checked) only when the stream's current lifetime is
   * `Living` — a plain `Slice`/`Burn` discard needs no confirmation.
   */
  confirm?: boolean;
};

export type DiscardResp = {
  stream_id: string;
  lifetime: Lifetime;
  worktree_removed: boolean;
  worktree_error?: string | null;
  room_destroyed: boolean;
  room_error?: string | null;
  /** A folder left in place, and why: it is, or holds, a project's root. */
  kept?: string | null;
  /** The chat's own folder went to the Trash. */
  chat_trashed: boolean;
};

export type FilesQuery = {
  stream_id: string;
  path?: string | null;
};

/** One entry of a `list_dir` listing — bare name, not a path. */
export type DirEntryInfo = {
  name: string;
  is_dir: boolean;
};

export type FileReadResp = {
  path: string;
  text: string;
  /**
   * `blake3` hex digest of `text`'s bytes — the baseline a human save
   * echoes back as `expected_digest` so a conflicting external change is
   * caught instead of overwritten (see `files_write`).
   */
  digest: string;
  /**
   * `text` is a secret (`machine_access::is_secret`): no desktop
   * screenshot while the app shows it.
   */
  secret: boolean;
};

export type FileWriteReq = {
  stream_id: string;
  path: string;
  text: string;
  /**
   * The digest last read for this file, or `None` for a file the caller
   * never read (a brand-new file). A `Some` that no longer matches what's
   * on disk comes back as a `conflict` response instead of overwriting it.
   */
  expected_digest?: string | null;
};

/**
 * Tagged so a conflict is an ordinary 200 body the client inspects (it
 * needs `current_text` to offer reload/diff), not an HTTP error status —
 * "someone else changed this file" is an expected outcome here, not a
 * route failure the way a jail escape or a discarded stream is.
 */
export type FileWriteResp = {
  path: string;
  digest: string;
  status: "ok";
} | {
  path: string;
  current_digest?: string | null;
  current_text?: string | null;
  /**
   * Who changed it since the save's base, in words ("the agent in
   * “Fix login”"); none when the Bakery doesn't know.
   */
  changed_by?: string | null;
  status: "conflict";
};

export type AgentTurnReq = {
  stream_id: string;
  /**
   * Optional now that tools exist: with a path the agent edits that file
   * directly; without one it explores the worktree first.
   */
  file_path?: string | null;
  instruction: string;
  /**
   * Which seat this turn answers as. `None` resolves to `Baker` for a
   * Baker stream, else `Orchestrator` (06-SEATS.md §2: "Baker stream uses
   * baker seat").
   */
  seat?: Seat | null;
  /**
   * Who actually issued this instruction — recorded verbatim as this
   * turn's `agent.turn.started` actor. `None` (every HTTP caller today:
   * the human-facing `/agent/turn` route, tests) means "a human typed
   * this," the historical default. `task_driver.rs` (07d-B Phase 4) is
   * the one caller that overrides it — to `task-driver` for a turn the
   * driver itself decided to issue (the opening contract, an autonomous
   * continuation, a re-queued proof failure), or to the actual actor of
   * a `task.message` it is relaying (`"human"`, `"agent:baker"`, ...) —
   * so the task's own transcript never misattributes the driver's
   * scheduling decisions to the operator who typed none of them.
   */
  actor?: string | null;
};

export type AttachmentUploadReq = {
  stream_id: string;
  seat: Seat;
  filename: string;
  media_type?: string;
  data_url: string;
  /**
   * The thread's own file it was read from (an @-mention, the selected
   * file): a file the node keeps secret is refused, as agents never get one.
   */
  source_path?: string | null;
};

export type AttachmentRef = {
  id: string;
  filename: string;
  media_type: string;
  bytes: number;
};

export type AttachmentQuery = {
  stream_id: string;
  id: string;
};

/**
 * Composer sends are durable follow-ups, not a request held open until a
 * model finishes.  Keeping this separate from the legacy `/agent/turn`
 * route preserves existing automation while letting a pane close, reload,
 * or submit another instruction during a long turn.
 */
export type AgentQueueReq = {
  stream_id: string;
  /**
   * Optional now that tools exist: with a path the agent edits that file
   * directly; without one it explores the worktree first.
   */
  file_path?: string | null;
  instruction: string;
  /**
   * Which seat this turn answers as. `None` resolves to `Baker` for a
   * Baker stream, else `Orchestrator` (06-SEATS.md §2: "Baker stream uses
   * baker seat").
   */
  seat?: Seat | null;
  /**
   * Who actually issued this instruction — recorded verbatim as this
   * turn's `agent.turn.started` actor. `None` (every HTTP caller today:
   * the human-facing `/agent/turn` route, tests) means "a human typed
   * this," the historical default. `task_driver.rs` (07d-B Phase 4) is
   * the one caller that overrides it — to `task-driver` for a turn the
   * driver itself decided to issue (the opening contract, an autonomous
   * continuation, a re-queued proof failure), or to the actual actor of
   * a `task.message` it is relaying (`"human"`, `"agent:baker"`, ...) —
   * so the task's own transcript never misattributes the driver's
   * scheduling decisions to the operator who typed none of them.
   */
  actor?: string | null;
  attachment_ids?: Array<string>;
  /**
   * Run this one turn read-only (Plan mode: read and search, no edits or
   * commands), whatever the conversation's mode: an Ask that must not
   * change files.
   */
  read_only?: boolean;
};

/**
 * The shape of `agent_control::Queued`: the queued message's event, and
 * `started`, the turn's own event, when an idle agent's turn began at once.
 */
export type QueuedShape = {
  stream_id: string;
  seq: number;
  content_hash: string;
  prev_hash?: string | null;
  actor: string;
  ts: string;
  causality: Array<string>;
  layer?: string | null;
  sensitivity: string;
  family: string;
  /** The event's payload, itself JSON text. */
  payload: string;
  event_hash: string;
  started?: EventShape | null;
};

export type ReviewTurnReq = {
  stream_id: string;
  turn_id: string;
  /** Keep every change (true), or restore every one (false). */
  accept: boolean;
};

export type TurnReviewed = {
  /** Changes decided now. */
  decided: number;
  /** Changes that couldn't be, each with why. */
  refused: Array<string>;
};

export type ChangeQuery = {
  stream_id: string;
  /** The change's review item (`agent.file.changed`). */
  event_hash: string;
};

export type ChangeContent = {
  file_path: string;
  /**
   * The file before the turn; `None` when the turn made it, or when it
   * isn't text.
   */
  before?: string | null;
  /**
   * The file the turn left; `None` when the turn removed it, or when it
   * isn't text.
   */
  after?: string | null;
  /**
   * A change kept out of the history (a credential, a denied path): no
   * desktop screenshot while the app shows it.
   */
  secret: boolean;
};

export type SnapshotLimit = {
  /**
   * A file bigger than this many MB gets no copy when a turn's changes
   * are recorded, so its change can only be kept, never undone.
   */
  max_mb: number;
};

export type SearchReq = {
  /** Plain text to find in file names and in files. */
  q: string;
  scope: Scope2;
  /** The conversation whose folder "this workspace" is. */
  stream_id?: string | null;
  /** Match case exactly; otherwise case is ignored. */
  exact_case?: boolean;
};

export type Scope2 = "workspace" | "everywhere";

export type Searched = {
  matches: Array<Found2>;
  /** It stopped at its most matches; a narrower search finds the rest. */
  truncated: boolean;
};

export type Found2 = {
  /** The folder it is in: a project's root, a chat, the workspace. */
  folder: string;
  /** That folder's project or chat, by name. */
  name: string;
  /** The file, relative to the folder. */
  path: string;
  /** The matching line, from 1; none when the file's name matched. */
  line?: number | null;
  /** That line, or the stretch of it around the match. */
  text?: string | null;
  /**
   * A secret file's line: `text` shows its key, its value masked;
   * `POST /search/reveal` shows it whole.
   */
  masked: boolean;
};

export type RevealReq = {
  /** The search it came from: its scope and conversation. */
  scope: Scope2;
  stream_id?: string | null;
  /** The match: its folder, its file (relative) and its line, as found. */
  folder: string;
  path: string;
  line: number;
};

export type Revealed = {
  /** The line, whole: the secret itself. */
  text: string;
  /**
   * Always: no desktop screenshot while the app shows it, the same
   * `secret` every reply carrying one has.
   */
  secret: boolean;
};

export type AgentTurnCancelReq = {
  stream_id: string;
  seat: Seat;
};

export type WithdrawReq = {
  stream_id: string;
  /** The queued message: its queued event's hash. */
  id: string;
};

export type Withdrawn = {
  instruction: string;
  file_path?: string | null;
  attachment_ids: Array<string>;
};

export type ToolPayloadQuery = {
  stream_id: string;
  sha: string;
};

export type AgentModeReq = {
  stream_id: string;
  seat: Seat;
  mode: OperatingMode;
};

/**
 * The composer capability mode, deliberately separate from the node-wide
 * write gate (`node::WriteMode`).  The mode travels with a queued message so
 * a later picker change cannot silently broaden an instruction already in
 * the durable queue.
 */
export type OperatingMode = "manual" | "plan" | "auto_edit" | "auto";

export type ToolsQuery = {
  stream_id: string;
};

export type Tools = {
  stream_id: string;
  tools: ToolsLimit;
};

export type ToolsReq = {
  stream_id: string;
  tools: ToolsLimit;
};

export type NativeConfigReq = {
  stream_id: string;
  seat: Seat;
  generation: number;
  config_id: string;
  value: NativeConfigInput;
};

export type NativeConfigInput = string | boolean;

export type SelectionReq = {
  stream_id: string;
  seat: Seat;
  /** The model, as the picker names it; absent or empty for the runtime's own. */
  model?: string | null;
  /** How the picker shows it, for the thread's "Model set to …" row. */
  model_label?: string | null;
  /** The effort, as the picker names it; absent or empty for the runtime's own. */
  effort?: string | null;
  effort_label?: string | null;
};

/** A selector's model and effort, by kind; `None` is the runtime's own. */
export type Selection = {
  model?: string | null;
  effort?: string | null;
};

export type AgentActivityQuery = {
  stream_id: string;
  seat: Seat;
};

/**
 * The wire shape `GET /agent/activity` returns and every activity
 * WebSocket push carries verbatim — see `main.rs`'s route registration and
 * `routes::agent`'s handler. `#[serde(tag = "phase", rename_all =
 * "snake_case")]` on [`ActivityPhase`] mirrors the existing
 * `bakery_agent::ModelCatalog`'s `#[serde(tag = "status", ...)]` precedent.
 */
export type AgentActivity = {
  agent_id: string;
  stream_id: string;
  turn_id: string;
  phase: ActivityPhase;
  /**
   * When `phase` became this kind of phase: a live "Thinking… 4s" counts
   * from it while contacting the model (W1-T).
   */
  phase_since: string;
  turn_started_at: string;
  recent_completed_tools: Array<CompletedTool>;
  /**
   * Always `None` this slice — real token/context numbers are
   * 07d-A3D1 Phase C's job; this field only carries the shape through
   * end to end so that phase has nothing left to plumb once it lands.
   */
  usage?: TurnUsage | null;
  /**
   * An estimate of the tokens this turn has produced so far, counted
   * from what streams (see [`estimate_tokens`]); the real figure only
   * arrives with the turn's end.
   */
  live_tokens: number;
  cancel_requested: boolean;
  /**
   * The operating mode this turn runs in (a read-only Ask's Plan, say),
   * which its MCP tool calls follow (`api_tools::Caller::mcp`).
   */
  mode?: OperatingMode | null;
};

export type ActivityPhase = {
  phase: "contacting_model";
} | {
  text: string;
  phase: "streaming";
} | {
  tool: string;
  call_id: string;
  started_at: string;
  kind: ToolRunKind;
  /**
   * A short, one-line preview of what the tool is actually doing — the
   * shell command for a Terminal/Execute call, the path for a file
   * tool — so the live work strip shows the real action instead of only
   * the tool's display name (which is how "running Terminal …" hid the
   * command that was stuck). `null` when the call carried no
   * inspectable input.
   */
  preview?: string | null;
  /**
   * The newest line a running command printed, when its runtime
   * streams output (a native command call); `null` otherwise.
   */
  output?: string | null;
  phase: "running_tool";
} | {
  kind: ApprovalKind;
  event_hash: string;
  phase: "awaiting_approval";
} | {
  note?: string | null;
  phase: "complete";
} | {
  reason: string;
  phase: "error";
} | {
  phase: "cancelled";
};

export type ToolRunKind = "tool" | "testing";

export type ApprovalKind = "file_edit" | "child_tool" | "question";

export type CompletedTool = {
  tool: string;
  call_id: string;
  ok: boolean;
  finished_at: string;
};

/**
 * See [`AgentActivity::usage`] — every field stays `None` until 07d-A3D1
 * Phase C populates it from what a provider's own response actually
 * reports.
 */
export type TurnUsage = {
  tokens_in?: number | null;
  tokens_out?: number | null;
  context_used?: number | null;
  context_window?: number | null;
  thought_tokens?: number | null;
  cache_read_tokens?: number | null;
  cache_write_tokens?: number | null;
  cost?: TurnCost | null;
  /** Wall time from the turn's start to its usage being recorded. */
  duration_ms?: number | null;
};

/** What one turn cost, as the runtime priced it. */
export type TurnCost = {
  amount: number;
  /** ISO 4217 code, as the runtime sent it ("USD"). */
  currency: string;
};

export type AgentAcceptReq = {
  stream_id: string;
  event_hash: string;
  /**
   * Human-edited replacement, never a path override. The original proposal
   * remains immutable; the acceptance records exactly what the human wrote.
   */
  new_content?: string | null;
};

export type AgentAnswerReq = {
  stream_id: string;
  /** The `agent.ask.pending` event being answered. */
  event_hash: string;
  /** One answer for all the questions, as text — what older clients send. */
  answer?: string;
  /** One answer per question, in order. Wins over `answer` when present. */
  answers?: Array<AskAnswer> | null;
};

/**
 * One question's answer: the options picked, and/or the human's own words.
 * Neither means that question was skipped.
 */
export type AskAnswer = {
  picks?: Array<string>;
  other?: string | null;
};

export type AgentResumeReq = {
  stream_id: string;
  seat?: Seat | null;
};

export type SessionChoiceReq = {
  stream_id: string;
  seat?: Seat | null;
  choice: SessionChoice;
};

export type SessionChoice = "retry" | "fresh";

export type AgentDecisionReq = {
  stream_id: string;
  event_hash: string;
};

/**
 * The permanent Baker's durable coordinates — what `GET /baker` returns
 * and what the Baker chrome renders. One per node, minted once by
 * [`ensure_permanent_baker`] and pointed at by `NodeConfig.baker_stream_id`.
 */
export type BakerHome = {
  stream_id: string;
  /** `{stream_id}:baker` — stable across restarts and model reassignment. */
  agent_id: string;
  /**
   * The read-scoped root this socket binds (the recorded
   * `bakery_source_root`), or `""` when the source root is unknown —
   * the identity exists either way (07d-B: identity is independent of
   * its execution environment).
   */
  root: string;
  /**
   * Latest `seat.model.assigned` for the baker seat on this stream, or
   * `None` when the human has not configured one yet. Never invented.
   */
  assigned_model_id?: string | null;
  /**
   * WIRE names (never the dotted spec `id` — see `BakerOpSpec`'s doc
   * comment) of every declared Bakery op this stream's turns can actually
   * call (07d-B Phase 2 item 5) — the same [`crate::baker_ops::BAKER_OPS`]
   * table `GET /baker/capabilities` and the turn's own system prompt
   * (`crate::agent_loop::format_baker_op_registry`) render. Names only;
   * summaries live at `/baker/capabilities`.
   */
  ops: Array<string>;
};

export type ListThreadsQuery = {
  include_archived?: boolean;
};

/**
 * One Baker conversation as the op registry and `GET /baker/threads` report
 * it. Deliberately carries no `owner_node_id` (the no-hostname rule every
 * route result keeps, `api_tools`).
 */
export type BakerThreadView = {
  stream_id: string;
  agent_id: string;
  main: boolean;
  title: string;
  archived: boolean;
  /** A turn is running on this conversation right now. */
  busy: boolean;
  last_ts: string;
  model_id?: string | null;
};

export type CreateThreadReq = {
  title?: string | null;
  /**
   * Optional first message, queued on the new thread's own durable queue
   * so it starts working immediately, in parallel with every other thread.
   */
  instruction?: string | null;
  /**
   * The runtime it runs on, a model_options id such as claude-native or
   * grok. Unset, the Baker's own. A Baker thread only: a peer runs on its
   * creator's runtime, model and effort.
   */
  runtime?: string | null;
  /**
   * The model, as the runtime's model picker names it. Unset, the
   * runtime's current one. A Baker thread only.
   */
  model?: string | null;
  /**
   * Reasoning effort, as the runtime names it (e.g. max). Unset, the
   * runtime's current one. A Baker thread only.
   */
  effort?: string | null;
  /**
   * Its operating mode. Unset, the mode of the conversation that opens it.
   * A peer's is at most its creator's.
   */
  mode?: OperatingMode | null;
  /**
   * A Baker thread works in its own git worktree, on a new baker/ branch
   * cut from its project's main, freshly fetched. A peer always gets its
   * own, from its creator's current HEAD.
   */
  worktree?: boolean;
  /**
   * An existing worktree of the Bakery's repo for it to work in, on the
   * branch it has: one the operator already keeps (never the main
   * checkout), adopted as the thread's own, so its agents write it. The
   * operator's alone to give; not with worktree. Discarding the thread
   * leaves the worktree where it was.
   */
  adopt?: string | null;
  /**
   * What its result must satisfy: text you judge, or {criterion, check},
   * where check is a shell command that proves it, run as a job when the
   * thread goes idle (exit 0 passes). Every result reports a verdict for
   * each. A Baker thread only.
   */
  acceptance?: Array<Criterion>;
  /**
   * What it may use of the Bakery's tools: all (the default), read or
   * none.
   */
  tools?: ToolsLimit | null;
  /**
   * The conversation opening it, when that is an agent other than the
   * Baker (filled in for one): the new conversation is its peer, in its
   * project, with at most its mode and Tools. The Baker names one to open
   * a peer of it: a workspace's worker, or a project's.
   */
  stream_id?: string | null;
};

/**
 * One thing a thread's result must satisfy: text the Baker judges itself,
 * or a criterion with a check that proves it.
 */
export type Criterion = string | {
  criterion: string;
  /**
   * A shell command, run as a proof job in the thread's workspace when
   * the thread goes idle after work; exit 0 passes.
   */
  check: string;
};

export type CreateThreadResp = {
  thread: BakerThreadView;
  /** The first message's durable queue record, when one was given. */
  queued?: EventShape | null;
  /** A caution about how it was opened. */
  note?: string | null;
};

export type RenameThreadReq = {
  stream_id: string;
  title: string;
};

export type ArchiveThreadReq = {
  stream_id: string;
  archived: boolean;
};

export type ThreadReadQuery = {
  stream_id: string;
  /** How many of the latest messages to return (default 20, at most 100). */
  limit?: number | null;
};

/** One conversation's work, as another agent reading it sees it. */
export type ThreadRead = {
  stream_id: string;
  title: string;
  /** running (a turn is under way or waiting to start), paused, or idle. */
  state: string;
  runtime?: string | null;
  model?: string | null;
  effort?: string | null;
  mode: OperatingMode;
  worktree: string;
  branch: string;
  queued: number;
  messages: Array<ThreadMessage>;
  /** Its latest finished turn; `None` until one has finished. */
  result?: TurnResult | null;
};

export type ThreadMessage = {
  /** operator, baker or agent. */
  from: string;
  text: string;
  at: string;
};

/**
 * A finished turn, after deer-flow's subagent status contract: status,
 * stop reason, result or error, model, tokens and acceptance.
 */
export type TurnResult = {
  /** completed, failed or cancelled. */
  status: string;
  /** turn_capped when its step limit ended it. */
  stop_reason?: string | null;
  turn_id: string;
  result: string;
  error?: string | null;
  model?: string | null;
  tokens?: Tokens | null;
  acceptance: Array<Acceptance>;
  finished_at: string;
};

export type Tokens = {
  input?: number | null;
  output?: number | null;
  total?: number | null;
};

export type Acceptance = {
  criterion: string;
  /**
   * passed or failed by its check's exit code; pending until the thread
   * goes idle and running while its proof job runs; unverified when it has
   * no check or its check could not finish (`detail` says why). Never
   * silently passed.
   */
  verdict: string;
  check?: string | null;
  /** Its proof job: job_output reads all of its output. */
  job_id?: string | null;
  exit_code?: number | null;
  /** A failed check's last output, or why it went unverified. */
  detail?: string | null;
};

export type ThreadWaitReq = {
  stream_id: string;
  /** The most seconds to wait (default and most 50). */
  timeout_secs?: number | null;
};

/** One conversation's work, as another agent reading it sees it. */
export type ThreadWaited = {
  /**
   * False when the wait ended with the thread still working: call
   * thread_wait again.
   */
  done: boolean;
  stream_id: string;
  title: string;
  /** running (a turn is under way or waiting to start), paused, or idle. */
  state: string;
  runtime?: string | null;
  model?: string | null;
  effort?: string | null;
  mode: OperatingMode;
  worktree: string;
  branch: string;
  queued: number;
  messages: Array<ThreadMessage>;
  /** Its latest finished turn; `None` until one has finished. */
  result?: TurnResult | null;
};

export type BoardQuery = {
  /** The conversation whose threads it shows. */
  stream_id: string;
};

export type Board = {
  /**
   * The conversation itself first, then the threads it opened, most
   * recently active first; for the Baker, its main conversation, then its
   * threads, then every other conversation in use.
   */
  threads: Array<BoardThread>;
};

export type BoardThread = {
  stream_id: string;
  title: string;
  state: ConversationStatus;
  /**
   * What it waits on, when it needs you or waits out a failure; why it
   * is idle (stopped, interrupted).
   */
  state_note?: string | null;
  /**
   * The last line of its last finished turn: its result's, or why it
   * failed or was stopped.
   */
  last_line?: string | null;
  /** Its own branch. */
  branch: string;
  /** Against main: the branch its repository's main checkout is on. */
  main?: Standing | null;
  /**
   * A workspace's worker, against the workspace's branch, where its work
   * goes first.
   */
  workspace?: Standing | null;
  /**
   * Why a standing is missing: it works in place, its branch is gone, or
   * git could not read it.
   */
  git_note?: string | null;
};

export type ConversationStatus = "working" | "needs_input" | "failed" | "done" | "idle";

/** Where a thread's branch stands against a branch its work goes into. */
export type Standing = {
  /** The branch its work goes into. */
  into: string;
  /** Commits of its branch that `into` lacks. */
  ahead: number;
  merged: MergedState;
  /** With commits to merge: they merge into `into` without a conflict. */
  mergeable?: boolean | null;
  /**
   * The files that would conflict: the first few (`git_merge_preview`
   * names them all).
   */
  conflicts: Array<string>;
  /**
   * What merging them would change on `into`: its files, and lines added
   * and removed.
   */
  files: number;
  added: number;
  removed: number;
};

/** Whether a branch has a thread's work, as git has it. */
export type MergedState = "no" | "yes" | "nothing_to_merge";

export type BakerAssignReq = {
  /** Human brief for this factory assignment. Required; drives the slug. */
  brief: string;
};

/** Dedicated Baker session: linked worktree on `baker/<slug>`, never in-place. */
export type BakerAssignResp = {
  /** `baker.assignment.accepted` — the assignment record for this call. */
  event: EventShape;
  context: ActiveContext;
  /** True when the Baker stream already existed (worktree reused). */
  attached: boolean;
  /** Node write gate. Default is Manual; first dogfood keeps propose→accept. */
  write_mode: WriteMode;
  brief: string;
  bakery_source_root: string;
};

export type BakerWorkList = {
  bakery_source_root: string;
  /** Default branch used as the merge base (e.g. main). */
  base?: string | null;
  items: Array<BakerWorkItem>;
  /** Set when listing git branches failed — items may still include streams. */
  git_error?: string | null;
  has_origin: boolean;
  gh_available: boolean;
};

/** One baker/* branch, optionally joined to a Heart stream. */
export type BakerWorkItem = {
  branch: string;
  stream_id?: string | null;
  worktree_path?: string | null;
  worktree_exists: boolean;
  brief?: string | null;
  /**
   * True when this branch's most recent known stream has been discarded —
   * distinct from `stream_id.is_none()` alone, which is also true for a
   * branch that never had any stream at all. 07c-LIVE-STATE: lets the
   * client tell the two apart without re-deriving repo-root equality
   * itself (see the comment at this field's call site in
   * `baker_work_list_inner`).
   */
  discarded: boolean;
};

export type BakerWorkDetailQuery = {
  /** `baker/<slug>` — preferred selector. */
  branch?: string | null;
  /** Alternative: resolve branch from stream onboard payload. */
  stream_id?: string | null;
};

/** Status + commits + diff vs base for one baker branch. No invented ahead/behind. */
export type BakerWorkDetail = {
  branch: string;
  base?: string | null;
  stream_id?: string | null;
  worktree_path?: string | null;
  worktree_exists: boolean;
  brief?: string | null;
  /** Commits on branch not reachable from base. Empty when none or git error. */
  commits: Array<CommitSummary>;
  commits_error?: string | null;
  /** `git diff --stat base...branch`. */
  diff_stat?: string | null;
  diff_error?: string | null;
  /** Uncommitted files in the linked worktree (if it exists on disk). */
  status: Array<StatusEntry>;
  status_error?: string | null;
  has_origin: boolean;
  gh_available: boolean;
  bakery_source_root: string;
};

/** One commit on `branch` that is not reachable from `base`. */
export type CommitSummary = {
  sha: string;
  subject: string;
};

export type BakerBranchReq = {
  branch: string;
};

export type BakerActionResp = {
  ok: boolean;
  message: string;
  branch: string;
};

export type BakerPrReq = {
  branch: string;
  title?: string | null;
  body?: string | null;
};

/** Merge requires an explicit human `confirm: true` — never agent-clickable text alone. */
export type BakerMergeReq = {
  branch: string;
  /** Must be true. Missing/false → refuse. */
  confirm?: boolean;
};

export type CapabilitiesQuery = {
  /**
   * Optional: the stream whose granted ops to render. Omitted (or naming
   * anything other than the permanent Baker) → `baker_ops` is empty.
   */
  stream_id?: string | null;
};

/**
 * `GET /baker/capabilities` response (07d-B Phase 2 item 5 — "render the
 * real table"): the canonical HTTP core tool set every HTTP/openai-compat
 * seat gets, plus (only when `stream_id` names the permanent Baker) the
 * declared Bakery op registry — "HTTP core set + ops the caller context
 * would get", not the legacy 5-tool `crate::tools::TOOLS` array this route
 * used to serialize verbatim.
 */
export type CapabilitiesView = {
  http_core: Array<CapabilityRow>;
  /**
   * Empty unless `stream_id` was given and names the permanent Baker
   * stream (`crate::baker_ops::is_permanent_baker_stream`) — an old
   * `/baker/assign` branch-job socket or a project socket gets none,
   * same authorization rule the tool loop itself enforces.
   */
  baker_ops: Array<BakerOpCapabilityRow>;
};

/**
 * One row of `GET /baker/capabilities`'s rendered table (name/summary/gate
 * class) — the same shape whether the row came from the canonical HTTP
 * core table or the Bakery op registry, so a client renders one list
 * without caring which table a row came from.
 */
export type CapabilityRow = {
  name: string;
  summary: string;
  gate_class: GateClass;
};

export type GateClass = "read" | "propose" | "exec" | "elevated";

/**
 * One row of the Bakery op half of the table — `sense`/`act` rather than a
 * [`crate::tools::GateClass`]: none of that enum's four variants honestly
 * describes "executes directly today, audited, reversible-by-retire" (07d-B
 * Phase 2 item 4's approval-routing for destructive ops is not built in
 * this pass — see `crate::baker_ops`'s module doc comment), so this row
 * makes no claim about gating it cannot back up.
 */
export type BakerOpCapabilityRow = {
  name: string;
  summary: string;
  sense: boolean;
};

export type TaskCreateReq = {
  parent_project_id: string;
  title: string;
  /**
   * Names the Baker work-order source (for A6: the spec path plus the
   * plan event hash) — free-form, never parsed beyond the idempotency
   * key it forms with the derived slug.
   */
  work_order: string;
  contract?: TaskContract;
};

/**
 * A task's completion contract (07d-B Phase 3 item 3) — advisory
 * coordination, not an enforcement primitive (item 6): `owned_files` is
 * declared, never checked against real writes, and `proof` is stored for
 * a human/Baker to run before signing off, never auto-invoked here.
 */
export type TaskContract = {
  scope?: string;
  /**
   * Advisory globs — coordination only, never enforced (no file-lock
   * primitive exists or is built here).
   */
  owned_files?: Array<string>;
  /**
   * Other tasks (by `task_id`, on the same parent project) that must
   * reach `done` before this one may start.
   */
  depends_on?: Array<string>;
  /**
   * argv lists a human/Baker runs before signing this task off — stored,
   * never auto-invoked by anything in this pass.
   */
  proof?: Array<Array<string>>;
  turn_budget?: number | null;
};

export type TaskCreateResp = {
  task: TaskView;
  /**
   * `false` when this call matched an existing `(work_order, slug)` and
   * returned it unchanged instead of minting a new one.
   */
  created: boolean;
};

export type TaskView = {
  task_id: string;
  title: string;
  slug: string;
  parent_project_id: string;
  work_order: string;
  contract: TaskContract;
  stream_id: string;
  status: TaskStatus;
  status_reason?: string | null;
  warnings: Array<string>;
  /**
   * Terminal: `task.retired` was appended for this task. `status` alone
   * cannot express this (there is no `TaskStatus::Retired` variant, and
   * retire does not rewrite the status that was current at the time), so
   * every consumer of this view — `GET /task/list`, `task_retire`'s own
   * response — must check `retired` rather than trust `status` to have
   * gone terminal on its own.
   */
  retired: boolean;
};

export type TaskStatus = "queued" | "running" | "blocked" | "awaiting_review" | "done" | "cancelled";

export type TaskStartReq = {
  project_id: string;
  task_id: string;
};

export type TaskStartResp = {
  task: TaskView;
  materialized: boolean;
  worktree_path?: string | null;
  branch?: string | null;
  base_commit?: string | null;
};

export type TaskMessageReq = {
  project_id: string;
  task_id: string;
  text: string;
};

export type TaskMessageResp = {
  event: EventShape;
};

export type TaskCancelReq = {
  project_id: string;
  task_id: string;
  reason?: string | null;
};

export type TaskCancelResp = {
  task: TaskView;
  /**
   * Whether an in-flight worker-seat turn's cancel flag was actually
   * flipped — `false` just means no turn was running, not that the
   * cancel failed: the task moves to `cancelled` either way.
   */
  turn_cancel_requested: boolean;
};

export type TaskRetireReq = {
  project_id: string;
  task_id: string;
};

export type TaskRetireResp = {
  task: TaskView;
  discard: DiscardResp;
};

export type TaskListQuery = {
  project_id: string;
};

export type TaskReviewQuery = {
  project_id: string;
  task_id: string;
};

export type TaskReviewResp = {
  task_id: string;
  base_commit?: string | null;
  files: Array<ReviewFile>;
  signed_off: boolean;
  signoff_commit?: string | null;
  /**
   * Content-derived identifier of exactly this reviewed state: blake3
   * over the base commit plus every file's (path, status, base-side blob
   * OID, new-side raw-byte hash) in sorted order. `POST /task/signoff`
   * requires it back and refuses when the workspace no longer matches —
   * concurrent changes are excluded, never silently swept in by
   * `git add -A`.
   */
  snapshot: string;
  /**
   * The project repo's currently checked-out branch — what
   * `/task/integrate` would merge into. The client shows it and sends it
   * back as the pinned `target_branch`; the server re-verifies at merge
   * time.
   */
  repo_current_branch?: string | null;
};

export type ReviewFile = {
  path: string;
  /**
   * `"added" | "modified" | "deleted" | "unreviewable"` —
   * `TaskReviewResp`'s spec-required name-status, absent from the
   * client's placeholder shape (07d-B Stage D: the tree state note says
   * to keep wire shapes compatible "or change them deliberately on both
   * sides" — this is the deliberate addition). `"unreviewable"` is this
   * pass's own addition: a git status letter this surface cannot render
   * as an old/new diff (today, only `U` — unmerged) still gets a visible
   * row instead of vanishing.
   */
  status: string;
  added: number;
  removed: number;
  old_content: string;
  new_content: string;
  /**
   * Why the displayed content is not the complete bytes, when it isn't
   * — `"binary"`, `"truncated for display"`, `"refused"`, `"oversize"`.
   * The UI renders this as an explicit chip; the snapshot below pins the
   * underlying bytes regardless of displayability.
   */
  note?: string | null;
  /**
   * `false` when the actual bytes could not even be hashed (jail/
   * credential refusal, unreadable, oversize) — sign-off refuses while
   * any such file is in the change set, because `git add -A` would
   * commit content nobody reviewed OR pinned. A binary file is
   * `true`: not displayable, but its exact bytes are hashed into the
   * snapshot, which is the explicit handling.
   */
  reviewable: boolean;
};

export type TaskSignoffReq = {
  project_id: string;
  task_id: string;
  confirm?: boolean;
  /**
   * The `TaskReviewResp.snapshot` the human actually reviewed. Required:
   * sign-off validates the workspace still matches it and refuses
   * otherwise — approving "whatever is on disk right now" is exactly the
   * unpinned behavior this field removes.
   */
  snapshot?: string;
};

export type TaskSignoffResp = {
  task: TaskView;
  signoff_commit: string;
};

export type TaskShipReq = {
  project_id: string;
  task_id: string;
  /**
   * Required by `/task/integrate` only — merging into the operator's
   * checkout is the one irreversible-feeling step of the three.
   */
  confirm?: boolean;
  /**
   * `/task/integrate` only, required: the branch the operator BELIEVES
   * they are merging into. Verified against the repo's actual checkout
   * at merge time — a checkout that switched branches between decision
   * and request refuses instead of silently landing the merge on the
   * wrong branch (review finding #4: no target assertion).
   */
  target_branch?: string | null;
  /** `/task/pr` only. */
  title?: string | null;
  body?: string | null;
};

export type TaskShipResp = {
  task_id: string;
  branch: string;
  detail: string;
};

/**
 * Measured factory rollup. Reuses Heart / gates / baker-work logic.
 * **No hostname, no secrets, no invented tokens/cost/email.**
 */
export type OperatorSnapshot = {
  write_mode: WriteMode;
  model: string;
  max_tool_steps?: number | null;
  stream_count: number;
  pending_gates: number;
  /** Sum of per-stream event counts (same substrate as GET /heart, no verify). */
  events: number;
  baker: OperatorBakerSlice;
};

export type OperatorBakerSlice = {
  /** False when source root unknown / not git — jobs empty, see `error`. */
  known: boolean;
  job_count: number;
  base?: string | null;
  has_origin?: boolean | null;
  gh_available?: boolean | null;
  jobs: Array<OperatorBakerJob>;
  error?: string | null;
};

/**
 * Compact baker job row for operators / agents — no absolute worktree paths
 * (those leak home layout) and no hostname.
 */
export type OperatorBakerJob = {
  branch: string;
  stream_id?: string | null;
  brief?: string | null;
  worktree_exists: boolean;
};

/** Public status only — never includes secret material. */
export type AuthStatus = {
  active_model: string;
  resolved_hint: string;
  xai: ProviderStatus;
  gemini: ProviderStatus;
  anthropic: ProviderStatus;
  openrouter: ProviderStatus;
};

export type ProviderStatus = {
  api_key_set: boolean;
  oauth_set: boolean;
  oauth_email?: string | null;
  model_id?: string | null;
};

/**
 * Codex/Cursor account status (07d-A3D1) — deliberately **not** folded into
 * [`auth_status`]/[`secrets::AuthStatus`]: that function is pure/sync and
 * asserted on directly by existing tests, and both these checks shell out
 * to the tool's own status subcommand, so a hung `codex`/`cursor-agent`
 * binary must never slow down every other provider row's poll too.
 */
export type CliStatusQuery = {
  force?: string | null;
};

export type CliAccountsStatus = {
  codex: CliAccountShape;
  cursor: CliAccountShape;
};

/** The shape of `bakery_agent::cli_tools::CliAccountStatus`. */
export type CliAccountShape = {
  installed: boolean;
  linked: boolean;
};

export type ClaudeNativeStatus = {
  installed: boolean;
  connected: boolean;
  /**
   * `subscription`, `signed_out`, `api_key`, `external`, `unknown`, or
   * `unavailable`.  API-key billing is deliberately not `connected`.
   */
  state: string;
  label?: string | null;
  reason?: string | null;
  adapter_version: string;
  adapter_integrity: string;
  metadata_provenance: string;
};

export type ApiKeyReq = {
  provider: string;
  key: string;
};

export type ClearReq = {
  provider: string;
};

export type ModelReq = {
  model: string;
};

export type DeviceStart = {
  user_code: string;
  verification_uri: string;
  verification_uri_complete: string;
  interval_seconds: number;
  expires_in_seconds: number;
  device_code: string;
};

export type DevicePollReq = {
  device_code: string;
};

export type DevicePollResult = {
  status: string;
  error?: string | null;
  email?: string | null;
};

export type ImportResp = {
  message: string;
  status: AuthStatus;
};

export type LoginStart = {
  verification_uri: string;
  built: boolean;
};

export type ClaudeCodeCompleteReq = {
  callback: string;
};

export type LoginResult = {
  status: string;
  error?: string | null;
};

export type PlugSecretReq = {
  plug_id: string;
  key: string;
};

export type PlugSecretStatus = {
  plug_id: string;
  secret_set: boolean;
};

export type PlugSecretClearReq = {
  plug_id: string;
};

export type PlugListResp = {
  acp_plugs: Array<AcpPlugConfig>;
  openai_compat_plugs: Array<OpenAiCompatPlugConfig>;
};

/**
 * One named ACP CLI plug (07d-A3B: "Named ACP plugs" — `acp:grok`,
 * `acp:claude`, `acp:codex`, ...). `id` is the operator-chosen model id a
 * seat is assigned (`SeatAssignment.model_id`) and the exact string Heart
 * events record as `model_id`. `argv[0]` is the binary; the rest is passed
 * after it (e.g. `["claude", "agent", "stdio"]` for a `claude` binary on
 * PATH — a bare name resolves the same way `std::process::Command` would,
 * an absolute path is exact). Distinct from the legacy singular
 * `acp_cli`/`acp_args`/`acp_config` trio, which stays live under the fixed
 * `"acp"` id for back-compat with an already-configured node.
 */
export type AcpPlugConfig = {
  id: string;
  label?: string | null;
  argv: Array<string>;
  config?: AcpConfigCopy | null;
  /**
   * Allowlisted directory env for the boxed child (`CODEX_HOME`, `AGY_BIN`,
   * `AGY_CONVERSATIONS_DIR`). Not a token dump.
   */
  env?: Record<string, string>;
  /**
   * Which MCP transport this CLI can be handed at `session/new`
   * (07d-B Phase 2 Stage C item 4) — `"stdio"` or `"none"`. Not merely
   * "supports some MCP configuration": a plug that cannot actually accept
   * the loopback MCP shim over stdio must say so, so
   * `routes::studio::model_ready_for_socket` can honestly refuse it for
   * the permanent Baker or a task socket rather than silently degrading
   * (every real CLI in this codebase today speaks stdio, hence the
   * default — `"none"` exists for a plug that is known not to).
   */
  mcp_transport?: string;
};

/**
 * One explicitly named directory copied into an ACP seat's isolated HOME.
 * The source is a host path selected by the operator; the destination is
 * relative to the seat HOME so a CLI can find it at its normal location
 * without inheriting or bind-mounting the host HOME.
 */
export type AcpConfigCopy = {
  source: string;
  home_relative_destination: string;
};

/**
 * One named openai-compat plug (07d-A3B: local/self-hosted OpenAI-shaped
 * endpoint — Ollama, llama.cpp, vLLM, HF TGI, an optional gateway sidecar).
 * `id` is the assignable model id, same role as [`AcpPlugConfig::id`]. The
 * bearer (if any) lives in `secrets.json`'s `openai_compat_secrets[id]`,
 * never here — this struct is not secret-shaped.
 */
export type OpenAiCompatPlugConfig = {
  id: string;
  label?: string | null;
  base_url: string;
  model: string;
};

export type AcpPlugCreateReq = {
  id: string;
  label?: string | null;
  argv: Array<string>;
  config?: AcpConfigCopy | null;
  env?: Record<string, string>;
  /** 07d-B Phase 2 Stage C item 4 — `"stdio"` (default) or `"none"`. */
  mcp_transport?: string;
};

export type AcpPlugUpdateReq = {
  id: string;
  label?: string | null;
  argv: Array<string>;
  config?: AcpConfigCopy | null;
  env?: Record<string, string>;
  /**
   * 07d-B Phase 2 Stage C item 4 — `"stdio"` or `"none"`. `None` here
   * (never a `"stdio"` default) means "the caller didn't send this field
   * at all" — `bakery-client`'s `acpPlugUpdate` today posts only
   * `{id, argv, label, config, env}` (no Providers-panel UI for this
   * field yet), and there is no other supported way to set
   * `mcp_transport: "none"` besides hand-editing `node.json`. Defaulting
   * a MISSING field to `"stdio"` used to silently overwrite that
   * hand-set `"none"` on the next unrelated edit (rename/argv fix) —
   * review finding — so a missing field must leave the stored value
   * alone; only an explicitly sent value may change it.
   */
  mcp_transport?: string | null;
};

export type PlugIdReq = {
  id: string;
};

export type OpenAiCompatPlugCreateReq = {
  id: string;
  label?: string | null;
  base_url: string;
  model: string;
};

export type OpenAiCompatPlugUpdateReq = {
  id: string;
  label?: string | null;
  base_url: string;
  model: string;
};

export type StudioState = {
  model_options: Array<ModelOption>;
  role_options: Array<RoleOption>;
  projects: Array<ProjectGroup>;
  /**
   * Sockets with no (or no longer resolvable — a retired/unknown
   * project_id) Project membership. Factory sockets are never here (see
   * `factory_sockets`) — 07d-A2's "Never list Baker's implementation
   * stream as a normal project socket" generalizes to: factory-kind
   * sockets never appear in Projects OR Ungrouped.
   */
  ungrouped_sockets: Array<SocketInfo>;
  /**
   * Every factory-kind socket, flat, ungrouped by design (07d-A2: Baker
   * job sockets are Floor content, not a sidebar tree section) — kept in
   * the wire payload so an already-open `/w/[sid]` workbench on one can
   * still resolve its own agents/busy/available_models.
   */
  factory_sockets: Array<SocketInfo>;
  /**
   * The picture's revision. A later answer with this same number is the
   * same picture: the window keeps what it is already showing.
   */
  revision: number;
};

export type ModelOption = {
  id: string;
  label: string;
  transport: string;
  node_available: boolean;
  reason?: string | null;
};

export type RoleOption = {
  id: string;
  label: string;
};

/**
 * One Project with its live roots and current member sockets. 07d-A3 §1:
 * grouping is by a socket's own `socket.project.set` fact alone, never by
 * `repo_root` — two sockets can share a Project despite pointing at
 * entirely different roots (or none).
 */
export type ProjectGroup = {
  project_id: string;
  name: string;
  pinned: boolean;
  roots: Array<RootView2>;
  sockets: Array<SocketInfo>;
  stacks: ProjectStacks;
};

export type RootView2 = {
  root_id: string;
  path: string;
  is_git: boolean;
};

/**
 * 07c-LIVE-STATE: `/studio/state` is now the client's *only* socket
 * inventory (`GET /streams` is no longer read for topology by any active
 * Studio surface) — so `SocketInfo` carries every field `StreamRef` does,
 * one-to-one, plus this route's own agent/model projection. Do not narrow
 * this back down; a second, incomplete wire shape is exactly what caused
 * the shadow `/streams` fetch this slice removed.
 *
 * 07d-A3: `project` is no longer this socket's own recorded git-repo name
 * verbatim — for a socket grouped under a Project, it is that Project's
 * CURRENT (possibly renamed) display name, resolved fresh on every
 * request; only an Ungrouped or factory socket falls back to its own
 * recorded name. `project_id`/`root_id` are `None` for both of those.
 */
export type SocketInfo = {
  stream_id: string;
  name: string;
  /** The name the operator gave it, shown in place of `name`. */
  title?: string | null;
  project: string;
  repo_root: string;
  branch: string;
  worktree_path: string;
  exists_on_disk: boolean;
  in_place: boolean;
  owner_node_id?: string | null;
  last_ts: string;
  events: number;
  lifetime: Lifetime;
  /**
   * Raw `stream.discarded` truth — `state` below is the same fact spelled
   * as a friendlier string; keep both so callers can match on whichever
   * is more convenient without re-deriving one from the other.
   */
  discarded: boolean;
  state: string;
  agents: Array<AgentInfo>;
  /**
   * Candidate models for this socket specifically, with per-socket
   * readiness — the picker's source of truth, not `model_options`'
   * node-wide `node_available`.
   */
  available_models: Array<SocketModelOption>;
  project_id?: string | null;
  root_id?: string | null;
  /**
   * Undecided write proposals on this exact socket — the
   * `routes::operator::gates_inner` projection filtered to `stream_id`
   * (07d-B Phase 4 item 3: "pending approvals come from the proposal/gate
   * projection"). Every socket kind gets this, not just tasks.
   */
  pending_gates: number;
  /**
   * Lines added/removed in the COMPLETE baseline diff (worktree vs
   * `task.workspace.materialized`, not the recorded-proposal log) for a
   * materialized task socket — `0`/`0` for every non-task socket, and for
   * a task with no materialized baseline yet (chat-only, not yet
   * started). Bounded-staleness cached
   * (`task_driver::changed_counts_cached`); the three review/sign-off/
   * integration surfaces never read this field, they recompute fresh.
   */
  changed_added: number;
  changed_removed: number;
  /**
   * Set on the permanent Baker's own socket (`main: true`) and on each of
   * its threads (docs/slices/BAKER-THREADS-03.md) — the Baker sidebar's
   * only source for its conversation list, so no second client cache.
   * `null` for every other socket, legacy `/baker/assign` jobs included.
   */
  baker_thread?: BakerThreadInfo | null;
  /**
   * Hidden from conversation lists — a Baker thread's own archive fact,
   * or `socket.archived` for any other conversation. Nothing is removed.
   */
  archived: boolean;
  /**
   * Where this conversation stands: working, needs_input, failed, done,
   * or idle — the same answer in every list that shows it.
   */
  status: ConversationStatus;
  /** One line on why: what it waits on, or how its last turn ended. */
  status_note?: string | null;
  /**
   * A chat with a folder of its own: Delete moves its files to the
   * Trash, and it can be made a project.
   */
  chat: boolean;
  /** The conversation that opened this one as its peer: a worker's orc. */
  opened_by?: string | null;
  /**
   * A workspace's orc: a conversation beneath the Baker on a worktree of
   * its own, whose workers name it in `opened_by`.
   */
  workspace: boolean;
};

export type AgentInfo = {
  agent_id: string;
  role_id: string;
  role_label: string;
  assigned_model_id?: string | null;
  /**
   * Claude native's own setting — "subscription" | "console" |
   * "openrouter" — so the picker can show it beside the model, e.g.
   * "Opus 5.5 · console". `None` for every other model.
   */
  assigned_provider?: string | null;
  /**
   * A switch asked for while this seat's turn ran, which lands when that
   * turn ends (`seat.model.pending`).
   */
  switching?: SeatSwitching | null;
  /**
   * The seat's last switch, when it couldn't land and nothing has moved
   * the seat since (`seat.model.switch_failed`).
   */
  switch_failed?: SeatSwitchFailed | null;
  socket_ready: boolean;
  /**
   * A turn is currently in flight for this agent (07d-A: "one
   * conversation + busy/run per agent_id, not per socket") — projected
   * from `state::is_turn_running`, not derived from any client-local flag.
   */
  busy: boolean;
  /**
   * What its picker shows on the Bakery's own loop, and every turn runs
   * on (`selection`): the model and effort chosen, none for the runtime's
   * own. A native seat's picker shows its session's instead.
   */
  selection: Selection;
  reason?: string | null;
};

/** Where a seat's waiting switch goes. */
export type SeatSwitching = {
  model_id: string;
  /**
   * Claude native's provider, as `AgentInfo::assigned_provider`; `None`
   * for every other model.
   */
  provider?: string | null;
};

/** A seat's switch that couldn't land: where it went, and why. */
export type SeatSwitchFailed = {
  /** Its `seat.model.pending`, by hash: which failure this is. */
  id: string;
  /** Where it went, as [`SeatSwitching`]. */
  to: SeatSwitching;
  error: string;
};

/**
 * One model's usability on THIS socket specifically — distinct from
 * `ModelOption.node_available`, which is node-wide. An HTTP model's
 * readiness never varies by socket; ACP's does (workspace + mise.toml).
 */
export type SocketModelOption = {
  id: string;
  ready: boolean;
  reason?: string | null;
};

/** `/studio/state`'s marker on a factory socket that is a Baker conversation. */
export type BakerThreadInfo = {
  /** The permanent Baker's own conversation — never archived or renamed. */
  main: boolean;
  title: string;
  archived: boolean;
};

export type ModelCatalogQuery = {
  id: string;
};

/**
 * A provider's model catalog, as [`Model::catalog`] reports it. `Available`
 * only ever holds models/limits/effort levels the provider's own live API
 * actually returned. `Unavailable` covers a subscription CLI or any
 * provider with nothing to enumerate — `default` is that model's own one
 * known id, never a guess at what else might exist.
 *
 * `#[serde(tag = "status", ...)]` gives the client a clean discriminated
 * union (`{"status":"available","models":[...]}` /
 * `{"status":"unavailable","default":{...},"reason":"..."}`) rather than an
 * awkward tuple encoding.
 */
export type ModelCatalog = {
  models: Array<ModelCatalogEntry>;
  status: "available";
} | {
  default: ModelCatalogEntry;
  reason: string;
  status: "unavailable";
};

/**
 * One selectable model within a provider's catalog — populated only from
 * that provider's own live list-models response, never fabricated.
 */
export type ModelCatalogEntry = {
  id: string;
  label: string;
  /**
   * Tokens, when the provider's own response reports one — `None` when it
   * doesn't (never backfilled from a static table).
   */
  context_window?: number | null;
  /** Empty when this specific model has no reasoning-effort concept. */
  effort_levels: Array<EffortLevel>;
};

/**
 * One selectable reasoning-effort level. Ids are provider-specific (e.g.
 * Anthropic/xAI: `low`/`medium`/`high`/`xhigh`/`max`) — never a shared
 * cross-provider enum, since the set differs per provider and per model.
 */
export type EffortLevel = {
  id: string;
  label: string;
};

export type AccountCapabilitiesQuery = {
  id: string;
  stream_id?: string | null;
  seat?: Seat | null;
  /**
   * Ask the adapter now instead of answering from memory — the picker's
   * Retry after a model list could not be read.
   */
  refresh?: boolean;
};

export type AccountCapabilities = {
  /**
   * `live_provider`, `subscription_adapter`, `native_session`, `remembered`
   * (what the account last reported, kept across restarts),
   * `configured_endpoint`, or `unavailable`. This lets a caller distinguish a real enumeration from
   * one known configured endpoint without parsing a prose reason.
   */
  source: string;
  catalog: ModelCatalog;
  /**
   * Transport-specific prompt attachment capabilities. For a native ACP
   * session these are the exact values advertised during setup, rather
   * than a UI guess about images, audio, or arbitrary binaries.
   */
  text_attachments: boolean;
  image_attachments: boolean;
  audio_attachments: boolean;
  /**
   * Bakery can enforce Plan on its HTTP tool loop. A native runtime can
   * enter Plan only when this exact ACP session advertises that mode;
   * otherwise Bakery refuses the selection before dispatch.
   */
  plan_enforceable: boolean;
  native_runtime?: AcpRuntimeSnapshot | null;
  availability_reason?: string | null;
  metadata_provenance: string;
  /**
   * This account has a model list to show. `false` for an arbitrary or
   * legacy id with no catalog concept at all — the picker then offers
   * nothing and says nothing, rather than reporting a failure.
   */
  catalog_expected: boolean;
};

/**
 * Wire shape for one tool in `GET /mcp/tools` — a local, Serialize-able
 * mirror of `bakery_agent::ToolDecl` (which deliberately carries no
 * `Serialize` impl of its own).
 */
export type McpToolWire = {
  name: string;
  description: string;
  /** Untyped JSON: no schema on the node describes it yet. */
  parameters: unknown;
};

export type McpCallReq = {
  name: string;
  /** Untyped JSON: no schema on the node describes it yet. */
  arguments?: unknown;
};

export type McpCallResp = {
  ok: boolean;
  result?: string | null;
  error?: string | null;
  /**
   * A screenshot the call took, base64 PNG, for the shim to hand over as
   * MCP image content; never part of `result`, which Heart records.
   */
  image_png?: string | null;
};

// ── Routes ──

/**
 * Whether the node is up.
 * GET /health · risk: read
 */
export type NodeHealthRequest = Record<string, never>;
export type NodeHealthResponse = string;
export function nodeHealth(req: NodeHealthRequest = {}, init?: RequestInit): Promise<NodeHealthResponse> {
  return request<NodeHealthResponse>("GET", "/health", req, init);
}

/**
 * This node's identity and policy.
 * GET /node · operator only
 */
export type NodeRequest = Record<string, never>;
export type NodeResponse = NodeInfo;
export function node(req: NodeRequest = {}, init?: RequestInit): Promise<NodeResponse> {
  return request<NodeResponse>("GET", "/node", req, init);
}

/**
 * What this node runs: Bakery build, Node.js, the Claude adapter and engine, each live session's runtime.
 * GET /versions · risk: read
 */
export type NodeVersionsRequest = Record<string, never>;
export type NodeVersionsResponse = Report;
export function nodeVersions(req: NodeVersionsRequest = {}, init?: RequestInit): Promise<NodeVersionsResponse> {
  return request<NodeVersionsResponse>("GET", "/versions", req, init);
}

/**
 * Every agent and its runtime, model, state and conversation.
 * GET /agents · risk: read
 */
export type AgentsRequest = Record<string, never>;
export type AgentsResponse = AgentsView;
export function agents(req: AgentsRequest = {}, init?: RequestInit): Promise<AgentsResponse> {
  return request<AgentsResponse>("GET", "/agents", req, init);
}

/**
 * The operator's node-wide policy per native tool kind: always, ask or never.
 * GET /agents/tool-policy · risk: read
 */
export type AgentsToolPolicyRequest = Record<string, never>;
export type AgentsToolPolicyResponse = Array<KindPolicy>;
export function agentsToolPolicy(req: AgentsToolPolicyRequest = {}, init?: RequestInit): Promise<AgentsToolPolicyResponse> {
  return request<AgentsToolPolicyResponse>("GET", "/agents/tool-policy", req, init);
}

/**
 * Set the node-wide native tool policy.
 * POST /agents/tool-policy · operator only
 */
export type AgentsToolPolicySetRequest = SetPolicy;
export type AgentsToolPolicySetResponse = Array<KindPolicy>;
export function agentsToolPolicySet(req: AgentsToolPolicySetRequest, init?: RequestInit): Promise<AgentsToolPolicySetResponse> {
  return request<AgentsToolPolicySetResponse>("POST", "/agents/tool-policy", req, init);
}

/**
 * Every "Always allow" an approval card made: a kind of action, allowed without asking in one project, in the Baker's conversations, or in one chat.
 * GET /agents/always-allowed · risk: read
 */
export type AgentsAlwaysAllowedRequest = Record<string, never>;
export type AgentsAlwaysAllowedResponse = Array<GrantView>;
export function agentsAlwaysAllowed(req: AgentsAlwaysAllowedRequest = {}, init?: RequestInit): Promise<AgentsAlwaysAllowedResponse> {
  return request<AgentsAlwaysAllowedResponse>("GET", "/agents/always-allowed", req, init);
}

/**
 * Take back one "Always allow": that kind asks again there.
 * POST /agents/always-allowed/remove · operator only
 */
export type AgentsAlwaysAllowedRemoveRequest = AlwaysGrant;
export type AgentsAlwaysAllowedRemoveResponse = Array<GrantView>;
export function agentsAlwaysAllowedRemove(req: AgentsAlwaysAllowedRemoveRequest, init?: RequestInit): Promise<AgentsAlwaysAllowedRemoveResponse> {
  return request<AgentsAlwaysAllowedRemoveResponse>("POST", "/agents/always-allowed/remove", req, init);
}

/**
 * The most open peers agents may open in one project, or one workspace.
 * GET /agents/peer-limit · risk: read
 */
export type AgentsPeerLimitRequest = Record<string, never>;
export type AgentsPeerLimitResponse = PeerLimit;
export function agentsPeerLimit(req: AgentsPeerLimitRequest = {}, init?: RequestInit): Promise<AgentsPeerLimitResponse> {
  return request<AgentsPeerLimitResponse>("GET", "/agents/peer-limit", req, init);
}

/**
 * Set the most open peers agents may open in one project, or one workspace.
 * POST /agents/peer-limit · operator only
 */
export type AgentsPeerLimitSetRequest = PeerLimit;
export type AgentsPeerLimitSetResponse = PeerLimit;
export function agentsPeerLimitSet(req: AgentsPeerLimitSetRequest, init?: RequestInit): Promise<AgentsPeerLimitSetResponse> {
  return request<AgentsPeerLimitSetResponse>("POST", "/agents/peer-limit", req, init);
}

/**
 * Start a new chat in its own folder; with model_id and instruction it starts working right away.
 * POST /chat/create · risk: run
 */
export type ChatCreateRequest = CreateChatReq;
export type ChatCreateResponse = ActiveContext;
export function chatCreate(req: ChatCreateRequest, init?: RequestInit): Promise<ChatCreateResponse> {
  return request<ChatCreateResponse>("POST", "/chat/create", req, init);
}

/**
 * Make a chat a project: its folder moves into Bakery's projects folder, or a folder chosen, as a Git repository, and the conversation carries on there.
 * POST /chat/make-project · operator only
 */
export type ChatMakeProjectRequest = MakeProjectReq;
export type ChatMakeProjectResponse = MadeProject;
export function chatMakeProject(req: ChatMakeProjectRequest, init?: RequestInit): Promise<ChatMakeProjectResponse> {
  return request<ChatMakeProjectResponse>("POST", "/chat/make-project", req, init);
}

/**
 * Archive (archived:true) or restore any conversation: a chat, a project socket or a Baker thread.
 * POST /conversation/archive · risk: write
 */
export type ConversationArchiveRequest = ArchiveReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ConversationArchiveResponse = unknown;
export function conversationArchive(req: ConversationArchiveRequest, init?: RequestInit): Promise<ConversationArchiveResponse> {
  return request<ConversationArchiveResponse>("POST", "/conversation/archive", req, init);
}

/**
 * Rename any conversation: a chat, a project's thread or a Baker thread; the main Baker keeps its name.
 * POST /conversation/rename · risk: write
 */
export type ConversationRenameRequest = RenameConversationReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ConversationRenameResponse = unknown;
export function conversationRename(req: ConversationRenameRequest, init?: RequestInit): Promise<ConversationRenameResponse> {
  return request<ConversationRenameResponse>("POST", "/conversation/rename", req, init);
}

/**
 * Stop a conversation's idle runtime; its next turn starts a fresh one with the whole history.
 * POST /conversation/restart · risk: write
 */
export type RuntimeRestartRequest = StreamReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type RuntimeRestartResponse = unknown;
export function runtimeRestart(req: RuntimeRestartRequest, init?: RequestInit): Promise<RuntimeRestartResponse> {
  return request<RuntimeRestartResponse>("POST", "/conversation/restart", req, init);
}

/**
 * Runtime home storage.
 * GET /agents/storage · operator only
 */
export type AgentsStorageRequest = Record<string, never>;
export type AgentsStorageResponse = StorageView;
export function agentsStorage(req: AgentsStorageRequest = {}, init?: RequestInit): Promise<AgentsStorageResponse> {
  return request<AgentsStorageResponse>("GET", "/agents/storage", req, init);
}

/**
 * List a runtime home folder.
 * GET /agents/storage/list · operator only
 */
export type AgentsStorageListRequest = PathQuery;
export type AgentsStorageListResponse = Array<Entry>;
export function agentsStorageList(req: AgentsStorageListRequest, init?: RequestInit): Promise<AgentsStorageListResponse> {
  return request<AgentsStorageListResponse>("GET", "/agents/storage/list", req, init);
}

/**
 * Read a runtime home file.
 * GET /agents/storage/file · operator only
 */
export type AgentsStorageFileRequest = PathQuery;
export type AgentsStorageFileResponse = FileView;
export function agentsStorageFile(req: AgentsStorageFileRequest, init?: RequestInit): Promise<AgentsStorageFileResponse> {
  return request<AgentsStorageFileResponse>("GET", "/agents/storage/file", req, init);
}

/**
 * Delete runtime home storage.
 * POST /agents/storage/delete · operator only
 */
export type AgentsStorageDeleteRequest = PathQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type AgentsStorageDeleteResponse = unknown;
export function agentsStorageDelete(req: AgentsStorageDeleteRequest, init?: RequestInit): Promise<AgentsStorageDeleteResponse> {
  return request<AgentsStorageDeleteResponse>("POST", "/agents/storage/delete", req, init);
}

/**
 * Which folders, programs and variables agents may reach, and what stays denied under any grant.
 * GET /machine-access · risk: read
 */
export type MachineAccessRequest = Record<string, never>;
export type MachineAccessResponse = View;
export function machineAccess(req: MachineAccessRequest = {}, init?: RequestInit): Promise<MachineAccessResponse> {
  return request<MachineAccessResponse>("GET", "/machine-access", req, init);
}

/**
 * Set machine access.
 * POST /machine-access · operator only
 */
export type MachineAccessSetRequest = MachineAccess;
export type MachineAccessSetResponse = Saved;
export function machineAccessSet(req: MachineAccessSetRequest, init?: RequestInit): Promise<MachineAccessSetResponse> {
  return request<MachineAccessSetResponse>("POST", "/machine-access", req, init);
}

/**
 * Probe the sandbox.
 * POST /machine-access/check · operator only
 */
export type MachineAccessCheckRequest = CheckReq;
export type MachineAccessCheckResponse = CheckResult;
export function machineAccessCheck(req: MachineAccessCheckRequest, init?: RequestInit): Promise<MachineAccessCheckResponse> {
  return request<MachineAccessCheckResponse>("POST", "/machine-access/check", req, init);
}

/**
 * Decide a folder access request.
 * POST /machine-access/requests/decide · operator only
 */
export type MachineAccessRequestsDecideRequest = DecideAccess;
export type MachineAccessRequestsDecideResponse = EventShape;
export function machineAccessRequestsDecide(req: MachineAccessRequestsDecideRequest, init?: RequestInit): Promise<MachineAccessRequestsDecideResponse> {
  return request<MachineAccessRequestsDecideResponse>("POST", "/machine-access/requests/decide", req, init);
}

/**
 * Set one agent's reach on the scope dial.
 * POST /machine-access/reach · operator only
 */
export type MachineAccessReachRequest = ReachReq;
export type MachineAccessReachResponse = ReachSet;
export function machineAccessReach(req: MachineAccessReachRequest, init?: RequestInit): Promise<MachineAccessReachResponse> {
  return request<MachineAccessReachResponse>("POST", "/machine-access/reach", req, init);
}

/**
 * Turn one agent's containers switch on or off.
 * POST /machine-access/containers · operator only
 */
export type MachineAccessContainersRequest = ContainersReq;
export type MachineAccessContainersResponse = ContainersSet;
export function machineAccessContainers(req: MachineAccessContainersRequest, init?: RequestInit): Promise<MachineAccessContainersResponse> {
  return request<MachineAccessContainersResponse>("POST", "/machine-access/containers", req, init);
}

/**
 * Set one container seat's CPUs and memory, or return it to the default sized from this Mac.
 * POST /machine-access/seat-size · operator only
 */
export type MachineAccessSeatSizeRequest = SeatSizeReq;
export type MachineAccessSeatSizeResponse = SeatSizeSet;
export function machineAccessSeatSize(req: MachineAccessSeatSizeRequest, init?: RequestInit): Promise<MachineAccessSeatSizeResponse> {
  return request<MachineAccessSeatSizeResponse>("POST", "/machine-access/seat-size", req, init);
}

/**
 * Sign Claude in for container seats, in a terminal pane.
 * POST /machine-access/claude-container/sign-in · operator only
 */
export type MachineAccessClaudeContainerSignInRequest = Record<string, never>;
export type MachineAccessClaudeContainerSignInResponse = TerminalInfo;
export function machineAccessClaudeContainerSignIn(req: MachineAccessClaudeContainerSignInRequest = {}, init?: RequestInit): Promise<MachineAccessClaudeContainerSignInResponse> {
  return request<MachineAccessClaudeContainerSignInResponse>("POST", "/machine-access/claude-container/sign-in", req, init);
}

/**
 * Whether the seat image is built from this node's recipe: missing, stale or current.
 * GET /containers/seat-image · risk: read
 */
export type ContainersSeatImageRequest = Record<string, never>;
export type ContainersSeatImageResponse = SeatImage;
export function containersSeatImage(req: ContainersSeatImageRequest = {}, init?: RequestInit): Promise<ContainersSeatImageResponse> {
  return request<ContainersSeatImageResponse>("GET", "/containers/seat-image", req, init);
}

/**
 * Build the seat image from this node's recipe, in a terminal pane the operator sees.
 * POST /containers/seat-image/build · risk: run
 */
export type SeatImageBuildRequest = Record<string, never>;
export type SeatImageBuildResponse = TerminalInfo;
export function seatImageBuild(req: SeatImageBuildRequest = {}, init?: RequestInit): Promise<SeatImageBuildResponse> {
  return request<SeatImageBuildResponse>("POST", "/containers/seat-image/build", req, init);
}

/**
 * Containers, images, volumes, networks or compose projects on Apple container or Docker, as rows both backends share; a container Bakery started names its role, conversation and seat.
 * GET /containers · risk: read
 */
export type ContainersListRequest = ListQuery;
export type ContainersListResponse = Listing;
export function containersList(req: ContainersListRequest, init?: RequestInit): Promise<ContainersListResponse> {
  return request<ContainersListResponse>("GET", "/containers", req, init);
}

/**
 * One container, image, volume, network or compose project, as the backend describes it.
 * GET /containers/inspect · risk: read
 */
export type ContainersInspectRequest = ItemQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersInspectResponse = unknown;
export function containersInspect(req: ContainersInspectRequest, init?: RequestInit): Promise<ContainersInspectResponse> {
  return request<ContainersInspectResponse>("GET", "/containers/inspect", req, init);
}

/**
 * A container's last log lines.
 * GET /containers/logs · risk: read
 */
export type ContainersLogsRequest = LogsQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersLogsResponse = unknown;
export function containersLogs(req: ContainersLogsRequest, init?: RequestInit): Promise<ContainersLogsResponse> {
  return request<ContainersLogsResponse>("GET", "/containers/logs", req, init);
}

/**
 * CPU, memory and I/O of every running container on a backend.
 * GET /containers/stats · risk: read
 */
export type ContainersStatsRequest = BackendQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersStatsResponse = unknown;
export function containersStats(req: ContainersStatsRequest, init?: RequestInit): Promise<ContainersStatsResponse> {
  return request<ContainersStatsResponse>("GET", "/containers/stats", req, init);
}

/**
 * Start a stopped container.
 * POST /containers/start · risk: run
 */
export type ContainersStartRequest = ContainerReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersStartResponse = unknown;
export function containersStart(req: ContainersStartRequest, init?: RequestInit): Promise<ContainersStartResponse> {
  return request<ContainersStartResponse>("POST", "/containers/start", req, init);
}

/**
 * Stop a running container.
 * POST /containers/stop · risk: run
 */
export type ContainersStopRequest = ContainerReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersStopResponse = unknown;
export function containersStop(req: ContainersStopRequest, init?: RequestInit): Promise<ContainersStopResponse> {
  return request<ContainersStopResponse>("POST", "/containers/stop", req, init);
}

/**
 * Restart a container, or a whole compose project (kind compose): it stops and starts again, its files kept. The Bakery's own are refused while in use and otherwise start fresh: an agent's sandbox with its next turn, a conversation's editor the next time Code opens.
 * POST /containers/restart · risk: run
 */
export type ContainersRestartRequest = ItemReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersRestartResponse = unknown;
export function containersRestart(req: ContainersRestartRequest, init?: RequestInit): Promise<ContainersRestartResponse> {
  return request<ContainersRestartResponse>("POST", "/containers/restart", req, init);
}

/**
 * Remove a stopped container, an image, a volume or a network, or take a compose project down.
 * POST /containers/remove · risk: destructive
 */
export type ContainersRemoveRequest = ItemReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersRemoveResponse = unknown;
export function containersRemove(req: ContainersRemoveRequest, init?: RequestInit): Promise<ContainersRemoveResponse> {
  return request<ContainersRemoveResponse>("POST", "/containers/remove", req, init);
}

/**
 * Remove every unused container, image, volume or network on a backend.
 * POST /containers/prune · risk: destructive
 */
export type ContainersPruneRequest = KindReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersPruneResponse = unknown;
export function containersPrune(req: ContainersPruneRequest, init?: RequestInit): Promise<ContainersPruneResponse> {
  return request<ContainersPruneResponse>("POST", "/containers/prune", req, init);
}

/**
 * Start a Docker compose project from its file, detached.
 * POST /containers/compose/up · risk: destructive
 */
export type ContainersComposeUpRequest = ComposeUpReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ContainersComposeUpResponse = unknown;
export function containersComposeUp(req: ContainersComposeUpRequest, init?: RequestInit): Promise<ContainersComposeUpResponse> {
  return request<ContainersComposeUpResponse>("POST", "/containers/compose/up", req, init);
}

/**
 * Start a container from an image, detached, its ports on 127.0.0.1 only; for an agent, what it can't reach stays out of its mounts.
 * POST /containers/run · risk: run
 */
export type ContainersRunRequest = RunReq;
export type ContainersRunResponse = Ran;
export function containersRun(req: ContainersRunRequest, init?: RequestInit): Promise<ContainersRunResponse> {
  return request<ContainersRunResponse>("POST", "/containers/run", req, init);
}

/**
 * Open a conversation's worktree in the operator's editor (openvscode-server in a container of its own) at a 127.0.0.1 URL that carries its connection token.
 * POST /code/open · operator only
 */
export type CodeOpenRequest = CodeReq;
export type CodeOpenResponse = ViewUrl;
export function codeOpen(req: CodeOpenRequest, init?: RequestInit): Promise<CodeOpenResponse> {
  return request<CodeOpenResponse>("POST", "/code/open", req, init);
}

/**
 * Stop a conversation's editor.
 * POST /code/close · operator only
 */
export type CodeCloseRequest = CodeReq;
export type CodeCloseResponse = Closed;
export function codeClose(req: CodeCloseRequest, init?: RequestInit): Promise<CodeCloseResponse> {
  return request<CodeCloseResponse>("POST", "/code/close", req, init);
}

/**
 * The scheme the app shows now (its colors as the app resolves them, light or dark, its code font), for every Code editor: open ones follow at once. The app sends it when its scheme or the Mac's light or dark mode changes.
 * POST /code/appearance · operator only
 */
export type CodeAppearanceRequest = AppearanceReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type CodeAppearanceResponse = unknown;
export function codeAppearance(req: CodeAppearanceRequest, init?: RequestInit): Promise<CodeAppearanceResponse> {
  return request<CodeAppearanceResponse>("POST", "/code/appearance", req, init);
}

/**
 * Open an edit row's file in its conversation's editor at the first line the edit changed, and say whether the file has changed since.
 * POST /code/goto · operator only
 */
export type CodeGotoRequest = GotoReq;
export type CodeGotoResponse = Goto;
export function codeGoto(req: CodeGotoRequest, init?: RequestInit): Promise<CodeGotoResponse> {
  return request<CodeGotoResponse>("POST", "/code/goto", req, init);
}

/**
 * A 127.0.0.1 URL for the dev server a conversation runs on a port, in its container or on this Mac.
 * POST /preview/open · risk: run
 */
export type PreviewOpenRequest = PreviewReq;
export type PreviewOpenResponse = ViewUrl;
export function previewOpen(req: PreviewOpenRequest, init?: RequestInit): Promise<PreviewOpenResponse> {
  return request<PreviewOpenResponse>("POST", "/preview/open", req, init);
}

/**
 * The model proxy (CLIProxyAPI): whether it's installed and running, and the hosts its upstream calls may reach.
 * GET /model-proxy · risk: read
 */
export type ModelProxyRequest = Record<string, never>;
export type ModelProxyResponse = ProxyStatus;
export function modelProxy(req: ModelProxyRequest = {}, init?: RequestInit): Promise<ModelProxyResponse> {
  return request<ModelProxyResponse>("GET", "/model-proxy", req, init);
}

/**
 * Install the pinned model proxy release, its sha256 checked, and start it.
 * POST /model-proxy/install · operator only
 */
export type ModelProxyInstallRequest = Record<string, never>;
export type ModelProxyInstallResponse = ProxyStatus;
export function modelProxyInstall(req: ModelProxyInstallRequest = {}, init?: RequestInit): Promise<ModelProxyInstallResponse> {
  return request<ModelProxyInstallResponse>("POST", "/model-proxy/install", req, init);
}

/**
 * Start the installed model proxy again, after fixing what stopped it.
 * POST /model-proxy/start · operator only
 */
export type ModelProxyStartRequest = Record<string, never>;
export type ModelProxyStartResponse = ProxyStatus;
export function modelProxyStart(req: ModelProxyStartRequest = {}, init?: RequestInit): Promise<ModelProxyStartResponse> {
  return request<ModelProxyStartResponse>("POST", "/model-proxy/start", req, init);
}

/**
 * Which providers the model proxy holds API keys for, and how many; never a key.
 * GET /model-proxy/keys · risk: read
 */
export type ModelProxyKeysRequest = Record<string, never>;
export type ModelProxyKeysResponse = ModelKeys;
export function modelProxyKeys(req: ModelProxyKeysRequest = {}, init?: RequestInit): Promise<ModelProxyKeysResponse> {
  return request<ModelProxyKeysResponse>("GET", "/model-proxy/keys", req, init);
}

/**
 * Give the model proxy an API key for a provider.
 * POST /model-proxy/keys · operator only
 */
export type ModelProxyKeysSetRequest = AddKey;
export type ModelProxyKeysSetResponse = ModelKeys;
export function modelProxyKeysSet(req: ModelProxyKeysSetRequest, init?: RequestInit): Promise<ModelProxyKeysSetResponse> {
  return request<ModelProxyKeysSetResponse>("POST", "/model-proxy/keys", req, init);
}

/**
 * Take an API key back from the model proxy, by its fingerprint.
 * POST /model-proxy/keys/remove · operator only
 */
export type ModelProxyKeysRemoveRequest = RemoveKey;
export type ModelProxyKeysRemoveResponse = ModelKeys;
export function modelProxyKeysRemove(req: ModelProxyKeysRemoveRequest, init?: RequestInit): Promise<ModelProxyKeysRemoveResponse> {
  return request<ModelProxyKeysRemoveResponse>("POST", "/model-proxy/keys/remove", req, init);
}

/**
 * The model proxy's numbers in Prometheus's text format: calls, tokens and latency by agent, provider and model, and calls another model answered.
 * GET /metrics · operator only
 */
export type MetricsRequest = Record<string, never>;
/** Untyped JSON: no schema on the node describes it yet. */
export type MetricsResponse = unknown;
export function metrics(req: MetricsRequest = {}, init?: RequestInit): Promise<MetricsResponse> {
  return request<MetricsResponse>("GET", "/metrics", req, init);
}

/**
 * Which conversation started which: the agent map.
 * GET /agents/map · risk: read
 */
export type AgentsMapRequest = Record<string, never>;
export type AgentsMapResponse = AgentMap;
export function agentsMap(req: AgentsMapRequest = {}, init?: RequestInit): Promise<AgentsMapResponse> {
  return request<AgentsMapResponse>("GET", "/agents/map", req, init);
}

/**
 * Ask npm for the newest releases of the node's runtimes.
 * POST /versions/check · risk: read
 */
export type VersionsCheckRequest = Record<string, never>;
export type VersionsCheckResponse = Report;
export function versionsCheck(req: VersionsCheckRequest = {}, init?: RequestInit): Promise<VersionsCheckResponse> {
  return request<VersionsCheckResponse>("POST", "/versions/check", req, init);
}

/**
 * The deployed Bakery build and the last deploy, restart or rollback.
 * GET /control/bakery · risk: read
 */
export type ControlBakeryRequest = Record<string, never>;
export type ControlBakeryResponse = ControlStatus;
export function controlBakery(req: ControlBakeryRequest = {}, init?: RequestInit): Promise<ControlBakeryResponse> {
  return request<ControlBakeryResponse>("GET", "/control/bakery", req, init);
}

/**
 * Deploy, deploy only the UI (the open app reloads, nothing restarts), restart or roll back Bakery.
 * POST /control/bakery · operator only
 */
export type ControlBakerySetRequest = ControlReq;
export type ControlBakerySetResponse = ControlStatus;
export function controlBakerySet(req: ControlBakerySetRequest, init?: RequestInit): Promise<ControlBakerySetResponse> {
  return request<ControlBakerySetResponse>("POST", "/control/bakery", req, init);
}

/**
 * Amphetamine on this Mac: its session, the time left, closed-display mode.
 * GET /mac/amphetamine · operator only
 */
export type MacAmphetamineRequest = Record<string, never>;
export type MacAmphetamineResponse = Amphetamine;
export function macAmphetamine(req: MacAmphetamineRequest = {}, init?: RequestInit): Promise<MacAmphetamineResponse> {
  return request<MacAmphetamineResponse>("GET", "/mac/amphetamine", req, init);
}

/**
 * Start or end an Amphetamine session, or turn its closed-display mode on or off, through Amphetamine's own scripting.
 * POST /mac/amphetamine · operator only
 */
export type MacAmphetamineSetRequest = AmphetamineReq;
export type MacAmphetamineSetResponse = Amphetamine;
export function macAmphetamineSet(req: MacAmphetamineSetRequest, init?: RequestInit): Promise<MacAmphetamineSetResponse> {
  return request<MacAmphetamineSetResponse>("POST", "/mac/amphetamine", req, init);
}

/**
 * Open Macs Fan Control, which has no other way in.
 * POST /mac/fans/open · operator only
 */
export type MacFansOpenRequest = Record<string, never>;
export type MacFansOpenResponse = null;
export function macFansOpen(req: MacFansOpenRequest = {}, init?: RequestInit): Promise<MacFansOpenResponse> {
  return request<MacFansOpenResponse>("POST", "/mac/fans/open", req, init);
}

/**
 * This Mac's disk, memory, CPU and heat, each with its level (ok, warn, bad), and the last ten minutes of CPU and memory.
 * GET /mac/meters · operator only
 */
export type MacMetersRequest = Record<string, never>;
export type MacMetersResponse = Meters;
export function macMeters(req: MacMetersRequest = {}, init?: RequestInit): Promise<MacMetersResponse> {
  return request<MacMetersResponse>("GET", "/mac/meters", req, init);
}

/**
 * This Mac's thermal state, hottest sensor and fans, and whether agents' new turns wait for it to cool.
 * GET /mac/heat · operator only
 */
export type MacHeatRequest = Record<string, never>;
export type MacHeatResponse = Heat;
export function macHeat(req: MacHeatRequest = {}, init?: RequestInit): Promise<MacHeatResponse> {
  return request<MacHeatResponse>("GET", "/mac/heat", req, init);
}

/**
 * Where this Mac's disk goes for the Bakery: free space, each thread's build folders, every image, container and volume on Docker and Apple container with the project that uses it, Colima's disk and the Bakery's own data.
 * GET /disk · operator only
 */
export type DiskRequest = Record<string, never>;
export type DiskResponse = DiskReport;
export function disk(req: DiskRequest = {}, init?: RequestInit): Promise<DiskResponse> {
  return request<DiskResponse>("GET", "/disk", req, init);
}

/**
 * One cleanup the operator picked and confirmed: a listed build folder, the images no container uses (never the Bakery's own), stopped containers, Docker's build cache, or the volumes they ticked by name.
 * POST /disk/clean · operator only
 */
export type DiskCleanRequest = CleanReq;
export type DiskCleanResponse = Cleaned;
export function diskClean(req: DiskCleanRequest, init?: RequestInit): Promise<DiskCleanResponse> {
  return request<DiskCleanResponse>("POST", "/disk/clean", req, init);
}

/**
 * What this Mac runs for the Bakery: Apple container, Tailscale and its machines, Colima, Homebrew's services, cloudflared, Supabase's local stacks; for Colima's default VM, what it uses inside, per container and of this Mac, its peak, a suggested size, the size waiting for its next start and its idle stop.
 * GET /engine · operator only
 */
export type EngineRequest = Record<string, never>;
export type EngineResponse = EngineRoom;
export function engine(req: EngineRequest = {}, init?: RequestInit): Promise<EngineResponse> {
  return request<EngineResponse>("GET", "/engine", req, init);
}

/**
 * Turn Colima's idle stop on or off: its default VM stops once nothing has used Docker for a while, and a Docker action starts it again.
 * POST /engine/colima/idle-stop · operator only
 */
export type EngineColimaIdleStopRequest = IdleStopReq;
export type EngineColimaIdleStopResponse = boolean;
export function engineColimaIdleStop(req: EngineColimaIdleStopRequest, init?: RequestInit): Promise<EngineColimaIdleStopResponse> {
  return request<EngineColimaIdleStopResponse>("POST", "/engine/colima/idle-stop", req, init);
}

/**
 * Pick the Bakery's Docker engine for projects that pick none: colima or orbstack (installed first); it answers what the switch means for images and volumes already there.
 * POST /engine/docker-default · operator only
 */
export type EngineDockerDefaultRequest = DockerDefaultReq;
export type EngineDockerDefaultResponse = DockerDefault;
export function engineDockerDefault(req: EngineDockerDefaultRequest, init?: RequestInit): Promise<EngineDockerDefaultResponse> {
  return request<EngineDockerDefaultResponse>("POST", "/engine/docker-default", req, init);
}

/**
 * The operator's yes to installing OrbStack with Homebrew; it installs in the background and the app hears how it went.
 * POST /engine/orbstack/install · operator only
 */
export type EngineOrbstackInstallRequest = Record<string, never>;
export type EngineOrbstackInstallResponse = string;
export function engineOrbstackInstall(req: EngineOrbstackInstallRequest = {}, init?: RequestInit): Promise<EngineOrbstackInstallResponse> {
  return request<EngineOrbstackInstallResponse>("POST", "/engine/orbstack/install", req, init);
}

/**
 * Pick the memory (and CPUs) Colima's default VM starts with next, or take a size back before it applies.
 * POST /engine/colima/size · operator only
 */
export type EngineColimaSizeRequest = ColimaSizeReq;
export type EngineColimaSizeResponse = ColimaSize | null;
export function engineColimaSize(req: EngineColimaSizeRequest, init?: RequestInit): Promise<EngineColimaSizeResponse> {
  return request<EngineColimaSizeResponse>("POST", "/engine/colima/size", req, init);
}

/**
 * Start a separate copy of one of this project's services (redis, postgres) for destructive work, with its own data and port, reachable by this thread alone. Stop it with service_copy_stop when done.
 * POST /project/service/copy · risk: write
 */
export type ServiceCopyRequest = ServiceCopyReq;
export type ServiceCopyResponse = ServiceCopy;
export function serviceCopy(req: ServiceCopyRequest, init?: RequestInit): Promise<ServiceCopyResponse> {
  return request<ServiceCopyResponse>("POST", "/project/service/copy", req, init);
}

/**
 * Stop this thread's copy of a service, by its port; its data goes with it.
 * POST /project/service/copy/stop · risk: write
 */
export type ServiceCopyStopRequest = ServiceStopReq;
export type ServiceCopyStopResponse = Array<ServiceCopy>;
export function serviceCopyStop(req: ServiceCopyStopRequest, init?: RequestInit): Promise<ServiceCopyStopResponse> {
  return request<ServiceCopyStopResponse>("POST", "/project/service/copy/stop", req, init);
}

/**
 * A project's secret-named files: the operator's answers on which its builds may read, and the files its rooms were refused that wait on them.
 * GET /files/read/asks · operator only
 */
export type FilesReadAsksRequest = SecretFilesQuery;
export type FilesReadAsksResponse = SecretFiles;
export function filesReadAsks(req: FilesReadAsksRequest, init?: RequestInit): Promise<FilesReadAsksResponse> {
  return request<FilesReadAsksResponse>("GET", "/files/read/asks", req, init);
}

/**
 * The operator's answer for a secret-named file a project's build needs: allow it (readable, never writable, in its rooms), deny it, or forget the answer. A private key takes sure: true.
 * POST /files/read/decide · operator only
 */
export type FilesReadDecideRequest = FileReadDecide;
export type FilesReadDecideResponse = SecretFiles;
export function filesReadDecide(req: FilesReadDecideRequest, init?: RequestInit): Promise<FilesReadDecideResponse> {
  return request<FilesReadDecideResponse>("POST", "/files/read/decide", req, init);
}

/**
 * A restart waiting for running turns goes ahead at once; the turns it cuts off carry on after it.
 * POST /control/restart/now · operator only
 */
export type ControlRestartNowRequest = Record<string, never>;
export type ControlRestartNowResponse = RestartWait | null;
export function controlRestartNow(req: ControlRestartNowRequest = {}, init?: RequestInit): Promise<ControlRestartNowResponse> {
  return request<ControlRestartNowResponse>("POST", "/control/restart/now", req, init);
}

/**
 * Client log line.
 * POST /client/log · operator only
 */
export type ClientLogRequest = ClientLog;
/** Untyped JSON: no schema on the node describes it yet. */
export type ClientLogResponse = unknown;
export function clientLog(req: ClientLogRequest, init?: RequestInit): Promise<ClientLogResponse> {
  return request<ClientLogResponse>("POST", "/client/log", req, init);
}

/**
 * Set the node-wide write mode.
 * POST /policy/write-mode · operator only
 */
export type PolicyWriteModeRequest = WriteModeReq;
export type PolicyWriteModeResponse = NodeInfo;
export function policyWriteMode(req: PolicyWriteModeRequest, init?: RequestInit): Promise<PolicyWriteModeResponse> {
  return request<PolicyWriteModeResponse>("POST", "/policy/write-mode", req, init);
}

/**
 * What a folder is: git repo, branch, worktrees, stacks. Changes nothing.
 * POST /project/inspect · risk: read
 */
export type ProjectInspectRequest = InspectReq;
export type ProjectInspectResponse = InspectResp;
export function projectInspect(req: ProjectInspectRequest, init?: RequestInit): Promise<ProjectInspectResponse> {
  return request<ProjectInspectResponse>("POST", "/project/inspect", req, init);
}

/**
 * Open a folder as a project socket (attach a branch or create a worktree).
 * POST /project/open · risk: write
 */
export type ProjectOpenRequest = OpenReq;
export type ProjectOpenResponse = OpenResp;
export function projectOpen(req: ProjectOpenRequest, init?: RequestInit): Promise<ProjectOpenResponse> {
  return request<ProjectOpenResponse>("POST", "/project/open", req, init);
}

/**
 * Create a Project: its own folder inside Bakery (root {mode:"bakery"}) or an existing folder ({mode:"attach",path}).
 * POST /project/create · risk: write
 */
export type ProjectCreateRequest = CreateProjectReq;
export type ProjectCreateResponse = CreateProjectResp;
export function projectCreate(req: ProjectCreateRequest, init?: RequestInit): Promise<ProjectCreateResponse> {
  return request<ProjectCreateResponse>("POST", "/project/create", req, init);
}

/**
 * Rename a Project; its project_id never changes.
 * POST /project/rename · risk: write
 */
export type ProjectRenameRequest = RenameProjectReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProjectRenameResponse = unknown;
export function projectRename(req: ProjectRenameRequest, init?: RequestInit): Promise<ProjectRenameResponse> {
  return request<ProjectRenameResponse>("POST", "/project/rename", req, init);
}

/**
 * A project's control plane, read from its files now: per folder what it's built with, its direct dependencies and tools; the MCP servers it defines plus the Bakery's own; its skills, once each; its instruction files; and the Bakery's own setup for it with what its files suggest. Every item names its source file.
 * GET /project/control · risk: read
 */
export type ProjectControlRequest = ControlQuery;
export type ProjectControlResponse = ControlPlane;
export function projectControl(req: ProjectControlRequest, init?: RequestInit): Promise<ProjectControlResponse> {
  return request<ProjectControlResponse>("GET", "/project/control", req, init);
}

/**
 * Set the Bakery's own setup for a project (toolchain manager, task runner, containers, secrets); each agent's next turn uses it.
 * POST /project/setup · operator only
 */
export type ProjectSetupRequest = SetupReq;
export type ProjectSetupResponse = ProjectStacks;
export function projectSetup(req: ProjectSetupRequest, init?: RequestInit): Promise<ProjectSetupResponse> {
  return request<ProjectSetupResponse>("POST", "/project/setup", req, init);
}

/**
 * A project's network list: the hosts its threads' sandboxes reach (the defaults, the hosts its files point to, and the operator's answers), each with where it comes from, and the hosts waiting on the operator.
 * GET /network/hosts · risk: read
 */
export type NetworkHostsRequest = HostsQuery;
export type NetworkHostsResponse = Hosts;
export function networkHosts(req: NetworkHostsRequest, init?: RequestInit): Promise<NetworkHostsResponse> {
  return request<NetworkHostsResponse>("GET", "/network/hosts", req, init);
}

/**
 * The operator's answer to a host for a project: allow, deny, or null to forget it so it asks again. It resolves each open ask it covers, in the thread that asked.
 * POST /network/host/decide · operator only
 */
export type NetworkHostDecideRequest = Decide;
export type NetworkHostDecideResponse = Hosts;
export function networkHostDecide(req: NetworkHostDecideRequest, init?: RequestInit): Promise<NetworkHostDecideResponse> {
  return request<NetworkHostDecideResponse>("POST", "/network/host/decide", req, init);
}

/**
 * Keep one of a Mac sandbox project's toolchain additions for good: the operator's own FVM, mise or gems gets it, for every project.
 * POST /project/toolchain/keep · operator only
 */
export type ProjectToolchainKeepRequest = KeepReq;
export type ProjectToolchainKeepResponse = Array<Addition>;
export function projectToolchainKeep(req: ProjectToolchainKeepRequest, init?: RequestInit): Promise<ProjectToolchainKeepResponse> {
  return request<ProjectToolchainKeepResponse>("POST", "/project/toolchain/keep", req, init);
}

/**
 * Ask the operator to install a toolchain or version into this Mac sandbox project's own copies: a Flutter version (tool flutter), a mise tool's version (tool mise, name node), or a gem. Their answer, and how the install went, come back to this conversation.
 * POST /project/toolchain/request · risk: read
 */
export type ToolchainRequestRequest = InstallReq;
export type ToolchainRequestResponse = string;
export function toolchainRequest(req: ToolchainRequestRequest, init?: RequestInit): Promise<ToolchainRequestResponse> {
  return request<ToolchainRequestResponse>("POST", "/project/toolchain/request", req, init);
}

/**
 * The operator's answer to an install ask: yes installs it into the project's own copies and tells the thread how it went; no tells the thread so.
 * POST /project/toolchain/decide · operator only
 */
export type ProjectToolchainDecideRequest = InstallDecide;
export type ProjectToolchainDecideResponse = Array<Install>;
export function projectToolchainDecide(req: ProjectToolchainDecideRequest, init?: RequestInit): Promise<ProjectToolchainDecideResponse> {
  return request<ProjectToolchainDecideResponse>("POST", "/project/toolchain/decide", req, init);
}

/**
 * A thread's door on this Mac, as a proxy URL for the app's Preview of it: the Preview then reaches only what the thread's project list allows.
 * GET /thread/door · operator only
 */
export type ThreadDoorRequest = DoorQuery;
export type ThreadDoorResponse = DoorUrl;
export function threadDoor(req: ThreadDoorRequest, init?: RequestInit): Promise<ThreadDoorResponse> {
  return request<ThreadDoorResponse>("GET", "/thread/door", req, init);
}

/**
 * The toolchains a thread's folder pins (mise's tools, Rust, FVM's Flutter, Xcode) beside what its own room runs, asked inside that room.
 * GET /thread/toolchains · operator only
 */
export type ThreadToolchainsRequest = ToolchainsQuery;
export type ThreadToolchainsResponse = Toolchains;
export function threadToolchains(req: ThreadToolchainsRequest, init?: RequestInit): Promise<ThreadToolchainsResponse> {
  return request<ThreadToolchainsResponse>("GET", "/thread/toolchains", req, init);
}

/**
 * Whether a thread has an unsigned iPhone build for Build to my iPhone, looked for where the button looks.
 * GET /thread/iphone · operator only
 */
export type ThreadIphoneRequest = IphoneBuildQuery;
export type ThreadIphoneResponse = IphoneBuild;
export function threadIphone(req: ThreadIphoneRequest, init?: RequestInit): Promise<ThreadIphoneResponse> {
  return request<ThreadIphoneResponse>("GET", "/thread/iphone", req, init);
}

/**
 * Run on my iPhone (as me): types flutter run for the thread's app, on the paired iPhone, into a new terminal of the operator's own in its worktree. Nothing runs until the operator presses Enter; then it runs as them.
 * POST /thread/iphone/run · operator only
 */
export type ThreadIphoneRunRequest = IphoneRunReq;
export type ThreadIphoneRunResponse = IphoneRunPrepared;
export function threadIphoneRun(req: ThreadIphoneRunRequest, init?: RequestInit): Promise<ThreadIphoneRunResponse> {
  return request<ThreadIphoneRunResponse>("POST", "/thread/iphone/run", req, init);
}

/**
 * Build to my iPhone: the thread's newest unsigned iOS build (flutter build ios --no-codesign), signed with the operator's own certificate for their provisioning profile and installed on their paired iPhone.
 * POST /thread/iphone/install · operator only
 */
export type ThreadIphoneInstallRequest = IphoneInstallReq;
export type ThreadIphoneInstallResponse = IphoneInstalled;
export function threadIphoneInstall(req: ThreadIphoneInstallRequest, init?: RequestInit): Promise<ThreadIphoneInstallResponse> {
  return request<ThreadIphoneInstallResponse>("POST", "/thread/iphone/install", req, init);
}

/**
 * Pin or unpin a Project in the sidebar.
 * POST /project/pin · risk: write
 */
export type ProjectPinRequest = PinProjectReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProjectPinResponse = unknown;
export function projectPin(req: ProjectPinRequest, init?: RequestInit): Promise<ProjectPinResponse> {
  return request<ProjectPinResponse>("POST", "/project/pin", req, init);
}

/**
 * Remove a Project from Bakery's lists; never deletes its files, roots, sockets, memory or history.
 * POST /project/retire · risk: write
 */
export type ProjectRetireRequest = RetireProjectReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProjectRetireResponse = unknown;
export function projectRetire(req: ProjectRetireRequest, init?: RequestInit): Promise<ProjectRetireResponse> {
  return request<ProjectRetireResponse>("POST", "/project/retire", req, init);
}

/**
 * Open a new directory-root socket on an existing Project.
 * POST /project/root/open · risk: write
 */
export type SocketOpenRequest = OpenRootReq;
export type SocketOpenResponse = ActiveContext;
export function socketOpen(req: SocketOpenRequest, init?: RequestInit): Promise<SocketOpenResponse> {
  return request<SocketOpenResponse>("POST", "/project/root/open", req, init);
}

/**
 * Attach an existing socket to a Project.
 * POST /project/socket/attach · risk: write
 */
export type ProjectSocketAttachRequest = AttachSocketReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProjectSocketAttachResponse = unknown;
export function projectSocketAttach(req: ProjectSocketAttachRequest, init?: RequestInit): Promise<ProjectSocketAttachResponse> {
  return request<ProjectSocketAttachResponse>("POST", "/project/socket/attach", req, init);
}

/**
 * Detach a socket from its Project.
 * POST /project/socket/detach · risk: write
 */
export type ProjectSocketDetachRequest = DetachSocketReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProjectSocketDetachResponse = unknown;
export function projectSocketDetach(req: ProjectSocketDetachRequest, init?: RequestInit): Promise<ProjectSocketDetachResponse> {
  return request<ProjectSocketDetachResponse>("POST", "/project/socket/detach", req, init);
}

/**
 * A Project at a glance: each repo's open pull requests (checks pass/fail/pending, review) and latest CI run per branch and workflow (gh), and each worktree's branch, lines added and removed since it forked, and whether it has uncommitted changes.
 * GET /project/rollup · risk: read
 */
export type ProjectRollupRequest = RollupQuery;
export type ProjectRollupResponse = ProjectRollup;
export function projectRollup(req: ProjectRollupRequest, init?: RequestInit): Promise<ProjectRollupResponse> {
  return request<ProjectRollupResponse>("GET", "/project/rollup", req, init);
}

/**
 * Every conversation (socket) this node knows, most recently active first.
 * GET /streams · risk: read
 */
export type SocketListRequest = Record<string, never>;
export type SocketListResponse = Array<StreamRef>;
export function socketList(req: SocketListRequest = {}, init?: RequestInit): Promise<SocketListResponse> {
  return request<SocketListResponse>("GET", "/streams", req, init);
}

/**
 * Every pending write proposal across the node, oldest first.
 * GET /gates · risk: read
 */
export type GatesListRequest = Record<string, never>;
export type GatesListResponse = Array<Gate>;
export function gatesList(req: GatesListRequest = {}, init?: RequestInit): Promise<GatesListResponse> {
  return request<GatesListResponse>("GET", "/gates", req, init);
}

/**
 * Event-log statistics for the node or one stream.
 * GET /heart · risk: read
 */
export type HeartRequest = HeartQuery;
export type HeartResponse = HeartStats;
export function heart(req: HeartRequest, init?: RequestInit): Promise<HeartResponse> {
  return request<HeartResponse>("GET", "/heart", req, init);
}

/**
 * A stream's durable memory entries.
 * GET /memory · risk: read
 */
export type MemoryReadRequest = StreamQuery;
export type MemoryReadResponse = MemoryView;
export function memoryRead(req: MemoryReadRequest, init?: RequestInit): Promise<MemoryReadResponse> {
  return request<MemoryReadResponse>("GET", "/memory", req, init);
}

/**
 * Write a durable memory note on a stream.
 * POST /memory/write · risk: write
 */
export type MemoryWriteRequest = MemoryWriteReq;
export type MemoryWriteResponse = EventShape;
export function memoryWrite(req: MemoryWriteRequest, init?: RequestInit): Promise<MemoryWriteResponse> {
  return request<MemoryWriteResponse>("POST", "/memory/write", req, init);
}

/**
 * The Bakery's toolkit: its skills, MCP servers, hooks, plugins and instructions, Bakery-wide and per project, each with how every runtime gets it.
 * GET /toolkit · risk: read
 */
export type ToolkitRequest = ToolkitQuery;
export type ToolkitResponse = Toolkit;
export function toolkit(req: ToolkitRequest, init?: RequestInit): Promise<ToolkitResponse> {
  return request<ToolkitResponse>("GET", "/toolkit", req, init);
}

/**
 * Add a toolkit item, or change one (with its id).
 * POST /toolkit · operator only
 */
export type ToolkitSetRequest = SetReq;
export type ToolkitSetResponse = ToolkitView;
export function toolkitSet(req: ToolkitSetRequest, init?: RequestInit): Promise<ToolkitSetResponse> {
  return request<ToolkitSetResponse>("POST", "/toolkit", req, init);
}

/**
 * Remove a toolkit item.
 * POST /toolkit/remove · operator only
 */
export type ToolkitRemoveRequest = IdReq;
export type ToolkitRemoveResponse = Toolkit;
export function toolkitRemove(req: ToolkitRemoveRequest, init?: RequestInit): Promise<ToolkitRemoveResponse> {
  return request<ToolkitRemoveResponse>("POST", "/toolkit/remove", req, init);
}

/**
 * Turn a toolkit item on or off.
 * POST /toolkit/enable · operator only
 */
export type ToolkitEnableRequest = EnableReq;
export type ToolkitEnableResponse = ToolkitView;
export function toolkitEnable(req: ToolkitEnableRequest, init?: RequestInit): Promise<ToolkitEnableResponse> {
  return request<ToolkitEnableResponse>("POST", "/toolkit/enable", req, init);
}

/**
 * The VS Code tool port: what the operator's Claude Code setup (~/.claude and ~/.claude.json) holds, read now: its instructions and rules, skills, MCP servers, hooks and plugins, and what has no Toolkit kind yet; each with what it becomes, the keys left out (by name), what changes on the way, and the Toolkit item it would change. The operator's only.
 * GET /toolkit/port · operator only
 */
export type ToolkitPortRequest = PortQuery;
export type ToolkitPortResponse = Port2;
export function toolkitPort(req: ToolkitPortRequest, init?: RequestInit): Promise<ToolkitPortResponse> {
  return request<ToolkitPortResponse>("GET", "/toolkit/port", req, init);
}

/**
 * Bring the ticked items of the operator's Claude Code setup into the Toolkit, Bakery-wide or for one project; one already there is changed. No key or secret comes across. The operator's only.
 * POST /toolkit/port · operator only
 */
export type ToolkitPortSetRequest = ImportReq;
export type ToolkitPortSetResponse = ImportResult;
export function toolkitPortSet(req: ToolkitPortSetRequest, init?: RequestInit): Promise<ToolkitPortSetResponse> {
  return request<ToolkitPortSetResponse>("POST", "/toolkit/port", req, init);
}

/**
 * The memory's learning loop: its settings (each part on or off, every trigger, time and limit, the models, the projects in it, the pause), what holds parts back now, the models it may ask, when the nightly run next starts, and each part's last run.
 * GET /learning · operator only
 */
export type LearningRequest = Record<string, never>;
export type LearningResponse = Learning;
export function learning(req: LearningRequest = {}, init?: RequestInit): Promise<LearningResponse> {
  return request<LearningResponse>("GET", "/learning", req, init);
}

/**
 * Change any of the learning loop's settings: what is sent is laid over them.
 * POST /learning · operator only
 */
export type LearningSetRequest = LoopSettings;
export type LearningSetResponse = Learning;
export function learningSet(req: LearningSetRequest, init?: RequestInit): Promise<LearningSetResponse> {
  return request<LearningSetResponse>("POST", "/learning", req, init);
}

/**
 * Run one part of the learning loop now (a thread-end review names its thread), whatever the pause and the time; it runs on behind the answer.
 * POST /learning/run · operator only
 */
export type LearningRunRequest = RunReq2;
export type LearningRunResponse = LearningRun;
export function learningRun(req: LearningRunRequest, init?: RequestInit): Promise<LearningRunResponse> {
  return request<LearningRunResponse>("POST", "/learning/run", req, init);
}

/**
 * The learning loop's runs, the newest first: what each did, the notes it touched, the proposals it made and the model calls it spent.
 * GET /learning/runs · operator only
 */
export type LearningRunsRequest = RunsQuery;
export type LearningRunsResponse = LearningRuns;
export function learningRuns(req: LearningRunsRequest, init?: RequestInit): Promise<LearningRunsResponse> {
  return request<LearningRunsResponse>("GET", "/learning/runs", req, init);
}

/**
 * Search the Bakery's memory: the notes you see (your thread's, your project's and the Bakery's) that match, best first, a line each with how much evidence it rests on and whether it is stale. Read the ones you need whole with memory_get.
 * POST /memory/search · risk: read
 */
export type MemorySearchRequest = MemorySearchReq;
export type MemorySearchResponse = MemoryFound;
export function memorySearch(req: MemorySearchRequest, init?: RequestInit): Promise<MemorySearchResponse> {
  return request<MemorySearchResponse>("POST", "/memory/search", req, init);
}

/**
 * Read notes whole, by id, within a token budget: title, summary, body, why, how to apply, links, evidence, versions and how each is used.
 * POST /memory/get · risk: read
 */
export type MemoryGetRequest = MemoryGetReq;
export type MemoryGetResponse = MemoryGot;
export function memoryGet(req: MemoryGetRequest, init?: RequestInit): Promise<MemoryGetResponse> {
  return request<MemoryGetResponse>("POST", "/memory/get", req, init);
}

/**
 * The notes linked to one, either way, one or two links away.
 * POST /memory/related · risk: read
 */
export type MemoryRelatedRequest = MemoryRelatedReq;
export type MemoryRelatedResponse = MemoryRelated;
export function memoryRelated(req: MemoryRelatedRequest, init?: RequestInit): Promise<MemoryRelatedResponse> {
  return request<MemoryRelatedResponse>("POST", "/memory/related", req, init);
}

/**
 * Every kept note, newest first, with how each is used and whether it is stale: the wiki's pages.
 * GET /memory/notes · risk: read
 */
export type MemoryNotesRequest = MemoryNotesQuery;
export type MemoryNotesResponse = MemoryNotes;
export function memoryNotes(req: MemoryNotesRequest, init?: RequestInit): Promise<MemoryNotesResponse> {
  return request<MemoryNotesResponse>("GET", "/memory/notes", req, init);
}

/**
 * One note's page: the note, how it is used, and every version.
 * GET /memory/note · risk: read
 */
export type MemoryNoteRequest = MemoryNoteQuery;
export type MemoryNoteResponse = NotePage;
export function memoryNote(req: MemoryNoteRequest, init?: RequestInit): Promise<MemoryNoteResponse> {
  return request<MemoryNoteResponse>("GET", "/memory/note", req, init);
}

/**
 * Propose a note for the Bakery's memory: only what will still matter in six months and is hard to find again from the code, git or the thread's history, resting on evidence. Never propose a secret; task progress, a todo or a log of finished work; what the code or its docs already say; the story of a one-off task; a setup failure the operator can fix (a missing tool, an unset credential); a claim that a tool or feature doesn't work; an error that went away on a retry (the retry is the lesson); or attempts that never worked (only a fix you know works). It waits for the operator, who keeps it, edits it or declines it; a repeat of a kept, waiting or declined note adds nothing.
 * POST /memory/propose · risk: write
 */
export type MemoryProposeRequest = ProposeReq;
export type MemoryProposeResponse = Proposed;
export function memoryPropose(req: MemoryProposeRequest, init?: RequestInit): Promise<MemoryProposeResponse> {
  return request<MemoryProposeResponse>("POST", "/memory/propose", req, init);
}

/**
 * Say whether a note you used helped or hurt the work; your latest word on a note counts.
 * POST /memory/rate · risk: write
 */
export type MemoryRateRequest = RateReq;
export type MemoryRateResponse = Rated;
export function memoryRate(req: MemoryRateRequest, init?: RequestInit): Promise<MemoryRateResponse> {
  return request<MemoryRateResponse>("POST", "/memory/rate", req, init);
}

/**
 * Notes agents proposed: those waiting for the operator (or kept, or declined), the oldest first.
 * GET /memory/proposals · risk: read
 */
export type MemoryProposalsRequest = ProposalsQuery;
export type MemoryProposalsResponse = MemoryProposals;
export function memoryProposals(req: MemoryProposalsRequest, init?: RequestInit): Promise<MemoryProposalsResponse> {
  return request<MemoryProposalsResponse>("GET", "/memory/proposals", req, init);
}

/**
 * Keep a proposed note, as it is or edited (a wider scope too), or decline it; a declined one never comes back.
 * POST /memory/review · operator only
 */
export type MemoryReviewRequest = ReviewReq;
export type MemoryReviewResponse = Reviewed;
export function memoryReview(req: MemoryReviewRequest, init?: RequestInit): Promise<MemoryReviewResponse> {
  return request<MemoryReviewResponse>("POST", "/memory/review", req, init);
}

/**
 * Keep every waiting proposal at once, as it is (or those named).
 * POST /memory/keep-all · operator only
 */
export type MemoryKeepAllRequest = KeepAllReq;
export type MemoryKeepAllResponse = KeptAll;
export function memoryKeepAll(req: MemoryKeepAllRequest, init?: RequestInit): Promise<MemoryKeepAllResponse> {
  return request<MemoryKeepAllResponse>("POST", "/memory/keep-all", req, init);
}

/**
 * Edit a note (a new version), or write a new one with its type, scope, title and body.
 * POST /memory/edit · operator only
 */
export type MemoryEditRequest = EditNoteReq;
export type MemoryEditResponse = NoteView;
export function memoryEdit(req: MemoryEditRequest, init?: RequestInit): Promise<MemoryEditResponse> {
  return request<MemoryEditResponse>("POST", "/memory/edit", req, init);
}

/**
 * Promote an agent's scratch note into a shared, kept note in a scope (bakery, baker, project:<id> or workspace:<thread>), edited if wanted; its versions stay.
 * POST /memory/promote · operator only
 */
export type MemoryPromoteRequest = PromoteReq;
export type MemoryPromoteResponse = NoteView;
export function memoryPromote(req: MemoryPromoteRequest, init?: RequestInit): Promise<MemoryPromoteResponse> {
  return request<MemoryPromoteResponse>("POST", "/memory/promote", req, init);
}

/**
 * Retire a note: it leaves the wiki and every search; its history stays.
 * POST /memory/retire · operator only
 */
export type MemoryRetireRequest = RetireReq;
export type MemoryRetireResponse = Retired;
export function memoryRetire(req: MemoryRetireRequest, init?: RequestInit): Promise<MemoryRetireResponse> {
  return request<MemoryRetireResponse>("POST", "/memory/retire", req, init);
}

/**
 * The memory's digest: proposals waiting for the operator (Keep all keeps them), notes gone stale, and the last day.
 * GET /memory/digest · risk: read
 */
export type MemoryDigestRequest = Record<string, never>;
export type MemoryDigestResponse = MemoryDigest;
export function memoryDigest(req: MemoryDigestRequest = {}, init?: RequestInit): Promise<MemoryDigestResponse> {
  return request<MemoryDigestResponse>("GET", "/memory/digest", req, init);
}

/**
 * What happened in the memory, the newest first: proposed, kept, edited, written, declined and retired.
 * GET /memory/timeline · risk: read
 */
export type MemoryTimelineRequest = TimelineQuery;
export type MemoryTimelineResponse = MemoryTimeline;
export function memoryTimeline(req: MemoryTimelineRequest, init?: RequestInit): Promise<MemoryTimelineResponse> {
  return request<MemoryTimelineResponse>("GET", "/memory/timeline", req, init);
}

/**
 * The backlog: what is to be done next, for the Bakery or a project, in the operator's order: each item's why, status (queued, doing, done, dropped), who added it, its threads and its history. An agent sees its project's items and the Bakery's.
 * GET /backlog · risk: read
 */
export type BacklogListRequest = BacklogQuery;
export type BacklogListResponse = Backlog;
export function backlogList(req: BacklogListRequest, init?: RequestInit): Promise<BacklogListResponse> {
  return request<BacklogListResponse>("GET", "/backlog", req, init);
}

/**
 * Add an item to the end of the backlog: something worth doing later, with why. An agent's goes into its project's backlog unless it names the scope (bakery, or a project), marked as its own.
 * POST /backlog/add · risk: write
 */
export type BacklogAddRequest = AddReq;
export type BacklogAddResponse = BacklogItem;
export function backlogAdd(req: BacklogAddRequest, init?: RequestInit): Promise<BacklogAddResponse> {
  return request<BacklogAddResponse>("POST", "/backlog/add", req, init);
}

/**
 * Change a backlog item's title, why, scope or linked threads.
 * POST /backlog/edit · risk: write
 */
export type BacklogEditRequest = EditReq;
export type BacklogEditResponse = BacklogItem;
export function backlogEdit(req: BacklogEditRequest, init?: RequestInit): Promise<BacklogEditResponse> {
  return request<BacklogEditResponse>("POST", "/backlog/edit", req, init);
}

/**
 * Move a backlog item along: queued, doing, done or dropped.
 * POST /backlog/status · risk: write
 */
export type BacklogStatusRequest = StatusReq;
export type BacklogStatusResponse = BacklogItem;
export function backlogStatus(req: BacklogStatusRequest, init?: RequestInit): Promise<BacklogStatusResponse> {
  return request<BacklogStatusResponse>("POST", "/backlog/status", req, init);
}

/**
 * Put a backlog item just above another, or at the end.
 * POST /backlog/move · operator only
 */
export type BacklogMoveRequest = MoveReq;
export type BacklogMoveResponse = Backlog;
export function backlogMove(req: BacklogMoveRequest, init?: RequestInit): Promise<BacklogMoveResponse> {
  return request<BacklogMoveResponse>("POST", "/backlog/move", req, init);
}

/**
 * Remove a backlog item.
 * POST /backlog/remove · operator only
 */
export type BacklogRemoveRequest = IdReq2;
export type BacklogRemoveResponse = Backlog;
export function backlogRemove(req: BacklogRemoveRequest, init?: RequestInit): Promise<BacklogRemoveResponse> {
  return request<BacklogRemoveResponse>("POST", "/backlog/remove", req, init);
}

/**
 * Read a skill your conversation has, by name: when one fits the work, read it before you start.
 * GET /toolkit/skill · risk: read
 */
export type SkillReadRequest = SkillQuery;
export type SkillReadResponse = Skill2;
export function skillRead(req: SkillReadRequest, init?: RequestInit): Promise<SkillReadResponse> {
  return request<SkillReadResponse>("GET", "/toolkit/skill", req, init);
}

/**
 * A conversation's brief: the standing instructions its agent gets every turn, who set it, whether an agent's still waits for the operator, and every change.
 * GET /conversation/brief · risk: read
 */
export type ConversationBriefRequest = BriefQuery;
export type ConversationBriefResponse = Brief;
export function conversationBrief(req: ConversationBriefRequest, init?: RequestInit): Promise<ConversationBriefResponse> {
  return request<ConversationBriefResponse>("GET", "/conversation/brief", req, init);
}

/**
 * Set another conversation's brief. The operator's holds as theirs; the Baker's, or an orchestrator's in the same project, is a recommendation, in effect until the operator turns it down. An agent never sets its own.
 * POST /conversation/brief · risk: write
 */
export type BriefSetRequest = SetBriefReq;
export type BriefSetResponse = Brief;
export function briefSet(req: BriefSetRequest, init?: RequestInit): Promise<BriefSetResponse> {
  return request<BriefSetResponse>("POST", "/conversation/brief", req, init);
}

/**
 * Keep, or turn down, an agent's brief; turned down, the brief before it holds again.
 * POST /conversation/brief/decide · operator only
 */
export type ConversationBriefDecideRequest = DecideBriefReq;
export type ConversationBriefDecideResponse = Brief;
export function conversationBriefDecide(req: ConversationBriefDecideRequest, init?: RequestInit): Promise<ConversationBriefDecideResponse> {
  return request<ConversationBriefDecideResponse>("POST", "/conversation/brief/decide", req, init);
}

/**
 * The operator's Claude Code sessions of the Bakery's own folders, newest first, to move into the Bakery: each one's transcript, its size, how often it was compacted, its title (its custom title first), the conversation it moved into, and where it can go on: a new workspace, a workspace's orc, a worker under one in its worktree, the Baker's own conversation or a Baker thread, the best fit suggested.
 * GET /move-in/sessions · operator only
 */
export type MoveInSessionsRequest = Record<string, never>;
export type MoveInSessionsResponse = ClaudeSessions;
export function moveInSessions(req: MoveInSessionsRequest = {}, init?: RequestInit): Promise<MoveInSessionsResponse> {
  return request<MoveInSessionsResponse>("GET", "/move-in/sessions", req, init);
}

/**
 * Move a Claude Code session into the Bakery with nothing lost, where the operator picks: one of its destinations (a new workspace's orc, a worker under a workspace in the worktree it has, the Baker's own conversation, a Baker thread, or a conversation already there). A copy of its transcript goes where that conversation's Claude runtime reads sessions, and its next turn resumes it. Again, what the original gained since comes along while the thread has not carried it on. Refused (409), with nothing changed: a conversation carrying on another session or in the middle of a turn, a session already moved in elsewhere, and one both sides went on with. The original is only read.
 * POST /move-in/import · operator only
 */
export type MoveInImportRequest = ImportReq2;
export type MoveInImportResponse = Imported2;
export function moveInImport(req: MoveInImportRequest, init?: RequestInit): Promise<MoveInImportResponse> {
  return request<MoveInImportResponse>("POST", "/move-in/import", req, init);
}

/**
 * A moved-in conversation's old conversation, read from the transcript copy its move-in made, as it came in: the operator's and Claude's messages, tool calls in short form (what a tool answered is never shown, only counted), compactions marked with their summary. Newest first, a page at a time: pass the last page's `earlier` as `before`. Keys and tokens are left out; what a page reads past without showing is counted in `skipped`, a line that can't be read says so, and a text cut short says how long it was.
 * GET /move-in/history · operator only
 */
export type MoveInHistoryRequest = MoveInHistoryQuery;
export type MoveInHistoryResponse = MoveInHistory;
export function moveInHistory(req: MoveInHistoryRequest, init?: RequestInit): Promise<MoveInHistoryResponse> {
  return request<MoveInHistoryResponse>("GET", "/move-in/history", req, init);
}

/**
 * What a fresh start would do now: the Baker thread it carries over (the one named, else the one called local models), the conversations it leaves behind in the old Heart, the Always-allow grants it clears, the worktrees left on disk, and why it can't go ahead (a turn running, or no single thread to keep).
 * GET /fresh-start/plan · operator only
 */
export type FreshStartPlanRequest = PlanQuery;
export type FreshStartPlanResponse = Plan;
export function freshStartPlan(req: FreshStartPlanRequest, init?: RequestInit): Promise<FreshStartPlanResponse> {
  return request<FreshStartPlanResponse>("GET", "/fresh-start/plan", req, init);
}

/**
 * Merge work waiting in the review where it goes: a chef's branch into main, as its spawner's thread_merge does (then the chef's thread is archived and its worktree and branch are removed, anything not committed going to the Trash first); a workspace's branch into its repository's main, or a workspace worker's into its workspace, only on the operator's approval and exactly as the review showed it. As a merge commit, into a clean checkout; a conflict merges nothing.
 * POST /thread/merge · operator only
 */
export type ThreadMergeRequest = MergeThreadReq;
export type ThreadMergeResponse = ThreadMerged;
export function threadMerge(req: ThreadMergeRequest, init?: RequestInit): Promise<ThreadMergeResponse> {
  return request<ThreadMergeResponse>("POST", "/thread/merge", req, init);
}

/**
 * Work waiting to merge, newest first: chefs' and workspaces' branches for main, and workspace workers' branches their orc proposed for its workspace. Each with its branch, what it merges into, its commits, the files it changes, and lines added and removed.
 * GET /merge/waiting · risk: read
 */
export type MergeWaitingRequest = Record<string, never>;
export type MergeWaitingResponse = MergeWaitingList;
export function mergeWaiting(req: MergeWaitingRequest = {}, init?: RequestInit): Promise<MergeWaitingResponse> {
  return request<MergeWaitingResponse>("GET", "/merge/waiting", req, init);
}

/**
 * Propose work for the merge review: a workspace's orc proposes a worker's branch for its workspace, or its own branch for main. Everything committed; a note says what it is and how it was tested. Asking again replaces it.
 * POST /merge/ready · risk: write
 */
export type MergeReadyRequest = MergeReadyReq;
export type MergeReadyResponse = MergeWaiting;
export function mergeReady(req: MergeReadyRequest, init?: RequestInit): Promise<MergeReadyResponse> {
  return request<MergeReadyResponse>("POST", "/merge/ready", req, init);
}

/**
 * Open a workspace: an orchestrator's conversation on a worktree of its own, a new branch off its repository's main, which opens workers as its peers. The Bakery's own beneath the Baker, or a catalog project's (project_id) on that project's repository; its work merges into that project's main through the merge review.
 * POST /workspace · risk: write
 */
export type WorkspaceRequest = CreateWorkspaceReq;
export type WorkspaceResponse = Workspace;
export function workspace(req: WorkspaceRequest, init?: RequestInit): Promise<WorkspaceResponse> {
  return request<WorkspaceResponse>("POST", "/workspace", req, init);
}

/**
 * What deleting a project, one repository of a project, a workspace, or a thread or chat would remove and what stays, changing nothing: its threads, the worktrees and branches the Bakery made (unsaved work in them kept in git first), rooms, containers, volumes, networks, the engines it could have used that are off (started to clear, then stopped), folders the Bakery made going to the Trash, the operator's own folders the trash_own tick takes, project roots let go, folders and branches kept because something staying uses them, why it can't go ahead (the main Baker, a turn running), and its digest.
 * POST /delete/plan · operator only
 */
export type DeletePlanRequest = DeletePlanReq;
export type DeletePlanResponse = DeletePlan;
export function deletePlan(req: DeletePlanRequest, init?: RequestInit): Promise<DeletePlanResponse> {
  return request<DeletePlanResponse>("POST", "/delete/plan", req, init);
}

/**
 * Carry out exactly the plan the operator saw, while a fresh plan still has its digest (409 when it changed, or is refused): unsaved work kept in git first (nothing goes that couldn't be kept), each thread discarded with its history kept, what the plan removes removed, engines it could have used that were off started and stopped again, folders to the Trash, a project retired. Recorded as delete.done; answers what went and each step that didn't.
 * POST /delete/confirm · operator only
 */
export type DeleteConfirmRequest = DeleteConfirmReq;
export type DeleteConfirmResponse = DeleteDone;
export function deleteConfirm(req: DeleteConfirmRequest, init?: RequestInit): Promise<DeleteConfirmResponse> {
  return request<DeleteConfirmResponse>("POST", "/delete/confirm", req, init);
}

/**
 * The Bakery's Trash: unsaved work deletes kept in git, in every repository the Bakery worked in, newest first: repo, name, when, its commits outside main and its uncommitted changes. Each stays until the operator empties the Trash.
 * GET /trash · operator only
 */
export type TrashRequest = Record<string, never>;
export type TrashResponse = TrashList;
export function trash(req: TrashRequest = {}, init?: RequestInit): Promise<TrashResponse> {
  return request<TrashResponse>("GET", "/trash", req, init);
}

/**
 * Put an entry of the Trash back: its branch made again at its tip and a worktree on it, its uncommitted changes restored exactly; it leaves the Trash.
 * POST /trash/put-back · operator only
 */
export type TrashPutBackRequest = TrashRef;
export type TrashPutBackResponse = PutBack;
export function trashPutBack(req: TrashPutBackRequest, init?: RequestInit): Promise<TrashPutBackResponse> {
  return request<TrashPutBackResponse>("POST", "/trash/put-back", req, init);
}

/**
 * Empty the Trash of the entries the operator saw and confirmed, one or all of them: the unsaved work in them is gone for good. Entries kept since are not touched; answers how many went and each that couldn't.
 * POST /trash/empty · operator only
 */
export type TrashEmptyRequest = EmptyReq;
export type TrashEmptyResponse = Emptied;
export function trashEmpty(req: TrashEmptyRequest, init?: RequestInit): Promise<TrashEmptyResponse> {
  return request<TrashEmptyResponse>("POST", "/trash/empty", req, init);
}

/**
 * Send a message or a note to another conversation, as SendMessage does between sessions: one in your project, any Baker conversation when you are one, or the Baker (to: baker). A message starts a turn there, after any turn running, so send one only when they must act on it. Anything else goes as a note (note: true): it shows there without starting a turn, and their next turn reads it. Your conversation shows it sent, then taken up or read.
 * POST /message/send · risk: write
 */
export type SendMessageRequest = SendReq;
export type SendMessageResponse = Sent;
export function sendMessage(req: SendMessageRequest, init?: RequestInit): Promise<SendMessageResponse> {
  return request<SendMessageResponse>("POST", "/message/send", req, init);
}

/**
 * The operator's saved replies, newest first: one conversation's, or every conversation's.
 * GET /saved · operator only
 */
export type SavedRequest = SavedQuery;
export type SavedResponse = SavedList;
export function saved(req: SavedRequest, init?: RequestInit): Promise<SavedResponse> {
  return request<SavedResponse>("GET", "/saved", req, init);
}

/**
 * Save a finished reply by its agent.turn.done event; saving it again answers the one already kept.
 * POST /saved · operator only
 */
export type SavedSetRequest = SaveReq;
export type SavedSetResponse = SavedReply;
export function savedSet(req: SavedSetRequest, init?: RequestInit): Promise<SavedSetResponse> {
  return request<SavedSetResponse>("POST", "/saved", req, init);
}

/**
 * Unsave a reply.
 * POST /saved/remove · operator only
 */
export type SavedRemoveRequest = SavedId;
export type SavedRemoveResponse = Unsaved;
export function savedRemove(req: SavedRemoveRequest, init?: RequestInit): Promise<SavedRemoveResponse> {
  return request<SavedRemoveResponse>("POST", "/saved/remove", req, init);
}

/**
 * Tick or untick one checklist item of a saved reply.
 * POST /saved/tick · operator only
 */
export type SavedTickRequest = TickReq;
export type SavedTickResponse = SavedReply;
export function savedTick(req: SavedTickRequest, init?: RequestInit): Promise<SavedTickResponse> {
  return request<SavedTickResponse>("POST", "/saved/tick", req, init);
}

/**
 * Import a folder of Claude Code memory notes into a conversation's memory (a Baker thread's into the one Baker memory), keeping each note's name, type, description and links.
 * POST /memory/import · operator only
 */
export type MemoryImportRequest = MemoryImportReq;
export type MemoryImportResponse = MemoryImported;
export function memoryImport(req: MemoryImportRequest, init?: RequestInit): Promise<MemoryImportResponse> {
  return request<MemoryImportResponse>("POST", "/memory/import", req, init);
}

/**
 * The context a stream's next turn starts from.
 * GET /context · risk: read
 */
export type ContextRequest = StreamQuery;
export type ContextResponse = ContextView;
export function context(req: ContextRequest, init?: RequestInit): Promise<ContextResponse> {
  return request<ContextResponse>("GET", "/context", req, init);
}

/**
 * Create a named git worktree for a project (legacy shape of project/open).
 * POST /worktree · risk: write
 */
export type WorktreeRequest = OpenWorktreeReq;
export type WorktreeResponse = EventShape;
export function worktree(req: WorktreeRequest, init?: RequestInit): Promise<WorktreeResponse> {
  return request<WorktreeResponse>("POST", "/worktree", req, init);
}

/**
 * Build a stream's sandbox room (its toolchain image).
 * POST /env/provision · risk: run
 */
export type EnvProvisionRequest = ProvisionReq;
export type EnvProvisionResponse = ProvisionResp;
export function envProvision(req: EnvProvisionRequest, init?: RequestInit): Promise<EnvProvisionResponse> {
  return request<EnvProvisionResponse>("POST", "/env/provision", req, init);
}

/**
 * Run one command (argv, no shell) in a stream's sandbox room and return its output.
 * POST /env/exec · risk: run
 */
export type EnvExecRequest = AttachReq;
export type EnvExecResponse = ExecResp;
export function envExec(req: EnvExecRequest, init?: RequestInit): Promise<EnvExecResponse> {
  return request<EnvExecResponse>("POST", "/env/exec", req, init);
}

/**
 * Interactive terminal attach.
 * POST /env/pty · operator only
 */
export type EnvPtyRequest = AttachReq;
export type EnvPtyResponse = ExecResp;
export function envPty(req: EnvPtyRequest, init?: RequestInit): Promise<EnvPtyResponse> {
  return request<EnvPtyResponse>("POST", "/env/pty", req, init);
}

/**
 * A stream's sandbox room status.
 * GET /env · risk: read
 */
export type EnvRequest = EnvQuery;
export type EnvResponse = EnvStatusResp;
export function env(req: EnvRequest, init?: RequestInit): Promise<EnvResponse> {
  return request<EnvResponse>("GET", "/env", req, init);
}

/**
 * Open an interactive terminal: a thread's (its stream_id, host or room), this Mac's from anywhere (stream_id sys:host: a login shell in the operator's home), or a project's (stream_id project:<id>: in its main folder, on this Mac, in its room or in a container as its sandbox setting says; mode is then ignored).
 * POST /terminal/create · operator only
 */
export type TerminalCreateRequest = CreateReq;
export type TerminalCreateResponse = TerminalInfo;
export function terminalCreate(req: TerminalCreateRequest, init?: RequestInit): Promise<TerminalCreateResponse> {
  return request<TerminalCreateResponse>("POST", "/terminal/create", req, init);
}

/**
 * The studio terminals open on a stream: a thread's, or project:<id> for a project's.
 * GET /terminal/list · risk: read
 */
export type TerminalListRequest = ListQuery2;
export type TerminalListResponse = Array<TerminalInfo>;
export function terminalList(req: TerminalListRequest, init?: RequestInit): Promise<TerminalListResponse> {
  return request<TerminalListResponse>("GET", "/terminal/list", req, init);
}

/**
 * Rename a studio terminal.
 * POST /terminal/rename · risk: write
 */
export type TerminalRenameRequest = RenameReq;
export type TerminalRenameResponse = TerminalInfo;
export function terminalRename(req: TerminalRenameRequest, init?: RequestInit): Promise<TerminalRenameResponse> {
  return request<TerminalRenameResponse>("POST", "/terminal/rename", req, init);
}

/**
 * What one of the thread's terminals printed last, as text, and whether something is running in it.
 * GET /terminal/read · risk: read
 */
export type TerminalReadRequest = ReadQuery;
export type TerminalReadResponse = TerminalText;
export function terminalRead(req: TerminalReadRequest, init?: RequestInit): Promise<TerminalReadResponse> {
  return request<TerminalReadResponse>("GET", "/terminal/read", req, init);
}

/**
 * Close a studio terminal and kill its process tree.
 * POST /terminal/close · risk: destructive
 */
export type TerminalCloseRequest = CloseReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type TerminalCloseResponse = unknown;
export function terminalClose(req: TerminalCloseRequest, init?: RequestInit): Promise<TerminalCloseResponse> {
  return request<TerminalCloseResponse>("POST", "/terminal/close", req, init);
}

/**
 * Terminal connection ticket.
 * POST /terminal/ticket · operator only
 */
export type TerminalTicketRequest = TicketReq;
export type TerminalTicketResponse = TicketResp;
export function terminalTicket(req: TerminalTicketRequest, init?: RequestInit): Promise<TerminalTicketResponse> {
  return request<TerminalTicketResponse>("POST", "/terminal/ticket", req, init);
}

/**
 * Jobs started with job_start, running or ended, newest first.
 * GET /jobs · risk: read
 */
export type JobListRequest = JobListQuery;
export type JobListResponse = Array<JobView>;
export function jobList(req: JobListRequest, init?: RequestInit): Promise<JobListResponse> {
  return request<JobListResponse>("GET", "/jobs", req, init);
}

/**
 * Start a long command (a build, tests, a dev server) in a conversation's sandbox and return at once. It shows live in a studio pane, and the conversation that started it (yours, when you call this) is told when it ends.
 * POST /jobs/start · risk: run
 */
export type JobStartRequest = JobStartReq;
export type JobStartResponse = JobView;
export function jobStart(req: JobStartRequest, init?: RequestInit): Promise<JobStartResponse> {
  return request<JobStartResponse>("POST", "/jobs/start", req, init);
}

/**
 * A job's output from an offset (since 0, then the last call's next), with its state and exit code.
 * GET /jobs/output · risk: read
 */
export type JobOutputRequest = JobOutputQuery;
export type JobOutputResponse = JobOutput;
export function jobOutput(req: JobOutputRequest, init?: RequestInit): Promise<JobOutputResponse> {
  return request<JobOutputResponse>("GET", "/jobs/output", req, init);
}

/**
 * Stop a running job.
 * POST /jobs/stop · risk: write
 */
export type JobStopRequest = JobStopReq;
export type JobStopResponse = JobView;
export function jobStop(req: JobStopRequest, init?: RequestInit): Promise<JobStopResponse> {
  return request<JobStopResponse>("POST", "/jobs/stop", req, init);
}

/**
 * A conversation's checkout: branch, upstream, ahead/behind, and each changed file, staged or not.
 * GET /git/status · risk: read
 */
export type GitStatusRequest = GitQuery;
export type GitStatusResponse = GitStatus;
export function gitStatus(req: GitStatusRequest, init?: RequestInit): Promise<GitStatusResponse> {
  return request<GitStatusResponse>("GET", "/git/status", req, init);
}

/**
 * Local branches with the current one marked, their upstream and tracking, and last commit.
 * GET /git/branches · risk: read
 */
export type GitBranchesRequest = GitQuery;
export type GitBranchesResponse = Array<Branch>;
export function gitBranches(req: GitBranchesRequest, init?: RequestInit): Promise<GitBranchesResponse> {
  return request<GitBranchesResponse>("GET", "/git/branches", req, init);
}

/**
 * Recent commits of the current branch or a named one.
 * GET /git/log · risk: read
 */
export type GitLogRequest = LogQuery;
export type GitLogResponse = Array<Commit>;
export function gitLog(req: GitLogRequest, init?: RequestInit): Promise<GitLogResponse> {
  return request<GitLogResponse>("GET", "/git/log", req, init);
}

/**
 * The diff of unstaged changes, staged ones, one file, one commit, or a whole branch against a base, what is not committed yet too if asked.
 * GET /git/diff · risk: read
 */
export type GitDiffRequest = DiffQuery;
export type GitDiffResponse = Diff;
export function gitDiff(req: GitDiffRequest, init?: RequestInit): Promise<GitDiffResponse> {
  return request<GitDiffResponse>("GET", "/git/diff", req, init);
}

/**
 * Stage paths, or every change; or take them out of the staged set.
 * POST /git/stage · risk: write
 */
export type GitStageRequest = StageReq;
export type GitStageResponse = GitStatus;
export function gitStage(req: GitStageRequest, init?: RequestInit): Promise<GitStageResponse> {
  return request<GitStageResponse>("POST", "/git/stage", req, init);
}

/**
 * Commit what is staged, with a message.
 * POST /git/commit · risk: write
 */
export type GitCommitRequest = CommitReq;
export type GitCommitResponse = Commit;
export function gitCommit(req: GitCommitRequest, init?: RequestInit): Promise<GitCommitResponse> {
  return request<GitCommitResponse>("POST", "/git/commit", req, init);
}

/**
 * Create a branch, from the current commit or a named one, and switch to it.
 * POST /git/branch · risk: write
 */
export type GitBranchRequest = BranchReq;
export type GitBranchResponse = GitStatus;
export function gitBranch(req: GitBranchRequest, init?: RequestInit): Promise<GitBranchResponse> {
  return request<GitBranchResponse>("POST", "/git/branch", req, init);
}

/**
 * Switch the checkout to another branch.
 * POST /git/switch · risk: write
 */
export type GitSwitchRequest = SwitchReq;
export type GitSwitchResponse = GitStatus;
export function gitSwitch(req: GitSwitchRequest, init?: RequestInit): Promise<GitSwitchResponse> {
  return request<GitSwitchResponse>("POST", "/git/switch", req, init);
}

/**
 * Fetch a remote's branches (default origin) without touching the checkout; status then shows ahead/behind.
 * POST /git/fetch · risk: read
 */
export type GitFetchRequest = FetchReq;
export type GitFetchResponse = GitStatus;
export function gitFetch(req: GitFetchRequest, init?: RequestInit): Promise<GitFetchResponse> {
  return request<GitFetchResponse>("POST", "/git/fetch", req, init);
}

/**
 * Bring the current branch up to its upstream with a merge, never a rebase; a conflict aborts it and changes nothing.
 * POST /git/pull · risk: write
 */
export type GitPullRequest = GitQuery;
export type GitPullResponse = GitStatus;
export function gitPull(req: GitPullRequest, init?: RequestInit): Promise<GitPullResponse> {
  return request<GitPullResponse>("POST", "/git/pull", req, init);
}

/**
 * Bring a thread up to the branch it forked from with a merge, never a rebase, fetching it first; a conflict stops it and lists the files to resolve.
 * POST /git/update · risk: write
 */
export type GitUpdateRequest = GitQuery;
export type GitUpdateResponse = Updated;
export function gitUpdate(req: GitUpdateRequest, init?: RequestInit): Promise<GitUpdateResponse> {
  return request<GitUpdateResponse>("POST", "/git/update", req, init);
}

/**
 * Rebase a thread onto a branch, with the reason you rebase instead of merging (git_update) recorded first; an agent's own git rebase is refused without one. A conflict stops it and lists the files, the rebase left in progress.
 * POST /git/rebase · risk: write
 */
export type GitRebaseRequest = RebaseReq;
export type GitRebaseResponse = Updated;
export function gitRebase(req: GitRebaseRequest, init?: RequestInit): Promise<GitRebaseResponse> {
  return request<GitRebaseResponse>("POST", "/git/rebase", req, init);
}

/**
 * Push a branch to origin and track it. Never forced.
 * POST /git/push · risk: destructive
 */
export type GitPushRequest = PushReq;
export type GitPushResponse = Done;
export function gitPush(req: GitPushRequest, init?: RequestInit): Promise<GitPushResponse> {
  return request<GitPushResponse>("POST", "/git/push", req, init);
}

/**
 * The operator's say on one change to Google Cloud with their own sign-in (anything but a read). The thread's door asks this itself when its room sends one, holds the request, and sends it once approved; calling it yourself sends nothing.
 * POST /google/change · risk: destructive
 */
export type GoogleChangeRequest = GoogleChange;
export type GoogleChangeResponse = string;
export function googleChange(req: GoogleChangeRequest, init?: RequestInit): Promise<GoogleChangeResponse> {
  return request<GoogleChangeResponse>("POST", "/google/change", req, init);
}

/**
 * Open a GitHub pull request for the current branch (gh).
 * POST /git/pr · risk: destructive
 */
export type GitPrRequest = PrReq;
export type GitPrResponse = Done;
export function gitPr(req: GitPrRequest, init?: RequestInit): Promise<GitPrResponse> {
  return request<GitPrResponse>("POST", "/git/pr", req, init);
}

/**
 * Merge a branch into the current one; a conflict aborts it and changes nothing.
 * POST /git/merge · risk: destructive
 */
export type GitMergeRequest = MergeReq;
export type GitMergeResponse = Done;
export function gitMerge(req: GitMergeRequest, init?: RequestInit): Promise<GitMergeResponse> {
  return request<GitMergeResponse>("POST", "/git/merge", req, init);
}

/**
 * What merging the current branch into base would change on base, its conflicts included, without touching the checkout (git merge-tree): each file with its lines added and removed (the first 200, all counted); with diff, the diff too, cut at 32 KiB.
 * GET /git/merge-preview · risk: read
 */
export type GitMergePreviewRequest = MergePreviewQuery;
export type GitMergePreviewResponse = MergePreview;
export function gitMergePreview(req: GitMergePreviewRequest, init?: RequestInit): Promise<GitMergePreviewResponse> {
  return request<GitMergePreviewResponse>("GET", "/git/merge-preview", req, init);
}

/**
 * Merge several branches into base in turn (base with the first, that with the next, and so on) without touching the checkout: each step's conflicts, so branches that conflict only with each other show up, then each file with its lines (the first 200, all counted); with diff, the combined diff too, cut at 32 KiB.
 * GET /git/merge-preview/chain · risk: read
 */
export type GitMergePreviewChainRequest = MergeChainQuery;
export type GitMergePreviewChainResponse = MergeChain;
export function gitMergePreviewChain(req: GitMergePreviewChainRequest, init?: RequestInit): Promise<GitMergePreviewChainResponse> {
  return request<GitMergePreviewChainResponse>("GET", "/git/merge-preview/chain", req, init);
}

/**
 * The checks on the current branch's pull request and how each stands (gh).
 * GET /git/pr/checks · risk: read
 */
export type GitPrChecksRequest = PrChecksQuery;
export type GitPrChecksResponse = PrChecks;
export function gitPrChecks(req: GitPrChecksRequest, init?: RequestInit): Promise<GitPrChecksResponse> {
  return request<GitPrChecksResponse>("GET", "/git/pr/checks", req, init);
}

/**
 * The operator's GitHub repositories and their organizations': visibility, default branch, when updated, and where each is cloned on this Mac, if it is.
 * GET /github/repos · risk: read
 */
export type GithubReposRequest = ReposQuery;
export type GithubReposResponse = Repos;
export function githubRepos(req: GithubReposRequest, init?: RequestInit): Promise<GithubReposResponse> {
  return request<GithubReposResponse>("GET", "/github/repos", req, init);
}

/**
 * Clone a GitHub repository into a new folder in the operator's home (~/Dev/code/<name> by default) and open it as a Project.
 * POST /github/clone · risk: write
 */
export type GithubCloneRequest = CloneReq;
export type GithubCloneResponse = Cloned;
export function githubClone(req: GithubCloneRequest, init?: RequestInit): Promise<GithubCloneResponse> {
  return request<GithubCloneResponse>("POST", "/github/clone", req, init);
}

/**
 * A repository's issues: open by default, most recent first.
 * GET /github/issues · risk: read
 */
export type GithubIssuesRequest = IssuesQuery;
export type GithubIssuesResponse = Array<Issue>;
export function githubIssues(req: GithubIssuesRequest, init?: RequestInit): Promise<GithubIssuesResponse> {
  return request<GithubIssuesResponse>("GET", "/github/issues", req, init);
}

/**
 * One issue with its body and comments.
 * GET /github/issue · risk: read
 */
export type GithubIssueRequest = IssueQuery;
export type GithubIssueResponse = IssueView;
export function githubIssue(req: GithubIssueRequest, init?: RequestInit): Promise<GithubIssueResponse> {
  return request<GithubIssueResponse>("GET", "/github/issue", req, init);
}

/**
 * Open an issue on a repository. It is published under the operator's name, so it always asks first.
 * POST /github/issue/create · risk: destructive
 */
export type GithubIssueCreateRequest = IssueCreateReq;
export type GithubIssueCreateResponse = Published;
export function githubIssueCreate(req: GithubIssueCreateRequest, init?: RequestInit): Promise<GithubIssueCreateResponse> {
  return request<GithubIssueCreateResponse>("POST", "/github/issue/create", req, init);
}

/**
 * Comment on an issue. It is published under the operator's name, so it always asks first.
 * POST /github/issue/comment · risk: destructive
 */
export type GithubIssueCommentRequest = IssueCommentReq;
export type GithubIssueCommentResponse = Published;
export function githubIssueComment(req: GithubIssueCommentRequest, init?: RequestInit): Promise<GithubIssueCommentResponse> {
  return request<GithubIssueCommentResponse>("POST", "/github/issue/comment", req, init);
}

/**
 * The pull request inbox's scope: the operator, their organizations and any other owner added, each on or off, and repositories turned on or off by name.
 * GET /github/scope · risk: read
 */
export type GithubScopeRequest = Record<string, never>;
export type GithubScopeResponse = GithubScope;
export function githubScope(req: GithubScopeRequest = {}, init?: RequestInit): Promise<GithubScopeResponse> {
  return request<GithubScopeResponse>("GET", "/github/scope", req, init);
}

/**
 * The operator turns owners and repositories on or off for the inbox; a login not listed yet is added.
 * POST /github/scope · operator only
 */
export type GithubScopeSetRequest = ScopeReq;
export type GithubScopeSetResponse = GithubScope;
export function githubScopeSet(req: GithubScopeSetRequest, init?: RequestInit): Promise<GithubScopeSetResponse> {
  return request<GithubScopeSetResponse>("POST", "/github/scope", req, init);
}

/**
 * The open pull requests across the inbox's scope, most recently updated first: checks, reviews, mergeability, and the thread and project each belongs to.
 * GET /github/prs · risk: read
 */
export type GithubPrsRequest = Record<string, never>;
export type GithubPrsResponse = Prs;
export function githubPrs(req: GithubPrsRequest = {}, init?: RequestInit): Promise<GithubPrsResponse> {
  return request<GithubPrsResponse>("GET", "/github/prs", req, init);
}

/**
 * Ask GitHub for the inbox's pull requests again now.
 * POST /github/prs/refresh · risk: read
 */
export type GithubPrsRefreshRequest = Record<string, never>;
export type GithubPrsRefreshResponse = Prs;
export function githubPrsRefresh(req: GithubPrsRefreshRequest = {}, init?: RequestInit): Promise<GithubPrsRefreshResponse> {
  return request<GithubPrsRefreshResponse>("POST", "/github/prs/refresh", req, init);
}

/**
 * One pull request in full: its text, files, diff, checks, reviews, review threads, comments and commits.
 * GET /github/pr · risk: read
 */
export type GithubPrRequest = PrQuery;
export type GithubPrResponse = PrView;
export function githubPr(req: GithubPrRequest, init?: RequestInit): Promise<GithubPrResponse> {
  return request<GithubPrResponse>("GET", "/github/pr", req, init);
}

/**
 * The operator reviews a pull request: a comment, an approval or a request for changes, with line comments.
 * POST /github/pr/review · operator only
 */
export type GithubPrReviewRequest = ReviewReq2;
export type GithubPrReviewResponse = Reviewed2;
export function githubPrReview(req: GithubPrReviewRequest, init?: RequestInit): Promise<GithubPrReviewResponse> {
  return request<GithubPrReviewResponse>("POST", "/github/pr/review", req, init);
}

/**
 * The operator comments on a pull request, or replies in one of its review threads.
 * POST /github/pr/comment · operator only
 */
export type GithubPrCommentRequest = CommentReq;
export type GithubPrCommentResponse = Commented;
export function githubPrComment(req: GithubPrCommentRequest, init?: RequestInit): Promise<GithubPrCommentResponse> {
  return request<GithubPrCommentResponse>("POST", "/github/pr/comment", req, init);
}

/**
 * The operator merges a pull request, or squashes it, deleting its branch if asked.
 * POST /github/pr/merge · operator only
 */
export type GithubPrMergeRequest = PrMergeReq;
export type GithubPrMergeResponse = Merged;
export function githubPrMerge(req: GithubPrMergeRequest, init?: RequestInit): Promise<GithubPrMergeResponse> {
  return request<GithubPrMergeResponse>("POST", "/github/pr/merge", req, init);
}

/**
 * The operator marks a draft pull request ready for review.
 * POST /github/pr/ready · operator only
 */
export type GithubPrReadyRequest = ReadyReq;
export type GithubPrReadyResponse = Ready;
export function githubPrReady(req: GithubPrReadyRequest, init?: RequestInit): Promise<GithubPrReadyResponse> {
  return request<GithubPrReadyResponse>("POST", "/github/pr/ready", req, init);
}

/**
 * The operator resolves a review thread, or opens it again.
 * POST /github/pr/thread/resolve · operator only
 */
export type GithubPrThreadResolveRequest = ResolveReq;
export type GithubPrThreadResolveResponse = Resolved;
export function githubPrThreadResolve(req: GithubPrThreadResolveRequest, init?: RequestInit): Promise<GithubPrThreadResolveResponse> {
  return request<GithubPrThreadResolveResponse>("POST", "/github/pr/thread/resolve", req, init);
}

/**
 * The latest CI run of each workflow on each branch, across the inbox's repositories and the operator's Projects'.
 * GET /github/runs · risk: read
 */
export type GithubRunsRequest = Record<string, never>;
export type GithubRunsResponse = Runs;
export function githubRuns(req: GithubRunsRequest = {}, init?: RequestInit): Promise<GithubRunsResponse> {
  return request<GithubRunsResponse>("GET", "/github/runs", req, init);
}

/**
 * One CI run with its jobs and their steps.
 * GET /github/run · risk: read
 */
export type GithubRunRequest = RunQuery;
export type GithubRunResponse = RunView;
export function githubRun(req: GithubRunRequest, init?: RequestInit): Promise<GithubRunResponse> {
  return request<GithubRunResponse>("GET", "/github/run", req, init);
}

/**
 * A CI job's log, step by step; each step keeps its last 64 KiB.
 * GET /github/run/log · risk: read
 */
export type GithubRunLogRequest = JobLogQuery;
export type GithubRunLogResponse = RunLog;
export function githubRunLog(req: GithubRunLogRequest, init?: RequestInit): Promise<GithubRunLogResponse> {
  return request<GithubRunLogResponse>("GET", "/github/run/log", req, init);
}

/**
 * The operator re-runs a CI run, or only its failed jobs.
 * POST /github/run/rerun · operator only
 */
export type GithubRunRerunRequest = RerunReq;
export type GithubRunRerunResponse = Queued;
export function githubRunRerun(req: GithubRunRerunRequest, init?: RequestInit): Promise<GithubRunRerunResponse> {
  return request<GithubRunRerunResponse>("POST", "/github/run/rerun", req, init);
}

/**
 * The grants a conversation holds: each lets it push one exact branch of one repository without asking first.
 * GET /grants · risk: read
 */
export type GrantsRequest = GrantsQuery;
export type GrantsResponse = Array<PushGrant>;
export function grants(req: GrantsRequest, init?: RequestInit): Promise<GrantsResponse> {
  return request<GrantsResponse>("GET", "/grants", req, init);
}

/**
 * The operator lets a conversation push a branch without asking first: its current branch by default, fixed to that name now.
 * POST /grants/give · operator only
 */
export type GrantsGiveRequest = GiveReq;
export type GrantsGiveResponse = PushGrant;
export function grantsGive(req: GrantsGiveRequest, init?: RequestInit): Promise<GrantsGiveResponse> {
  return request<GrantsGiveResponse>("POST", "/grants/give", req, init);
}

/**
 * Every remote action waiting for the operator across all conversations (pushes, pull requests, issues and comments), with their count.
 * GET /remote/waiting · risk: read
 */
export type RemoteWaitingRequest = Record<string, never>;
export type RemoteWaitingResponse = WaitingList;
export function remoteWaiting(req: RemoteWaitingRequest = {}, init?: RequestInit): Promise<RemoteWaitingResponse> {
  return request<RemoteWaitingResponse>("GET", "/remote/waiting", req, init);
}

/**
 * The whole Bakery's remote actions done, newest first: pushes, pull requests, issues, comments, reviews, merges and re-runs, with who did them.
 * GET /remote/history · risk: read
 */
export type RemoteHistoryRequest = HistoryQuery;
export type RemoteHistoryResponse = Array<RemoteAction>;
export function remoteHistory(req: RemoteHistoryRequest, init?: RequestInit): Promise<RemoteHistoryResponse> {
  return request<RemoteHistoryResponse>("GET", "/remote/history", req, init);
}

/**
 * End a grant: the operator takes it back, or the conversation's own agent gives it up.
 * POST /grants/revoke · risk: write
 */
export type GrantsRevokeRequest = EndReq;
export type GrantsRevokeResponse = Array<PushGrant>;
export function grantsRevoke(req: GrantsRevokeRequest, init?: RequestInit): Promise<GrantsRevokeResponse> {
  return request<GrantsRevokeResponse>("POST", "/grants/revoke", req, init);
}

/**
 * For each hunk a branch committed since base (base...HEAD), the recorded write that put it there: conversation, turn and tool call, with how many of its lines that write accounts for; none for a shell command, a hand edit or code from base.
 * GET /git/provenance · risk: read
 */
export type GitProvenanceRequest = ProvenanceQuery;
export type GitProvenanceResponse = Provenance;
export function gitProvenance(req: GitProvenanceRequest, init?: RequestInit): Promise<GitProvenanceResponse> {
  return request<GitProvenanceResponse>("GET", "/git/provenance", req, init);
}

/**
 * The app's layout: workspaces, each a strip of columns of panels, plus the sidebar and the Baker dock. Null until one is saved.
 * GET /ui/layout · risk: read
 */
export type UiLayoutRequest = Record<string, never>;
export type UiLayoutResponse = Layout | null;
export function uiLayout(req: UiLayoutRequest = {}, init?: RequestInit): Promise<UiLayoutResponse> {
  return request<UiLayoutResponse>("GET", "/ui/layout", req, init);
}

/**
 * Save the app's layout; an open app follows it.
 * POST /ui/layout · risk: write
 */
export type UiLayoutSetRequest = SaveLayoutReq;
export type UiLayoutSetResponse = Layout;
export function uiLayoutSet(req: UiLayoutSetRequest, init?: RequestInit): Promise<UiLayoutSetResponse> {
  return request<UiLayoutSetResponse>("POST", "/ui/layout", req, init);
}

/**
 * The app's appearance: its themes and the one in use. Null until one is saved.
 * GET /ui/appearance · risk: read
 */
export type UiAppearanceRequest = Record<string, never>;
/** Untyped JSON: no schema on the node describes it yet. */
export type UiAppearanceResponse = unknown;
export function uiAppearance(req: UiAppearanceRequest = {}, init?: RequestInit): Promise<UiAppearanceResponse> {
  return request<UiAppearanceResponse>("GET", "/ui/appearance", req, init);
}

/**
 * Save the app's appearance; an open app follows it.
 * POST /ui/appearance · risk: write
 */
export type UiAppearanceSetRequest = SaveAppearanceReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type UiAppearanceSetResponse = unknown;
export function uiAppearanceSet(req: UiAppearanceSetRequest, init?: RequestInit): Promise<UiAppearanceSetResponse> {
  return request<UiAppearanceSetResponse>("POST", "/ui/appearance", req, init);
}

/**
 * A conversation's own build of the app's UI (its bakery-client/build, from `pnpm --dir bakery-client build`): the hash the app checks it against, its size, the branch and commit it was built on, whether bakery-client has uncommitted changes, and the UI the app shows now.
 * GET /ui/bundle · risk: read
 */
export type UiBundleRequest = BundleQuery;
export type UiBundleResponse = Bundle;
export function uiBundle(req: UiBundleRequest, init?: RequestInit): Promise<UiBundleResponse> {
  return request<UiBundleResponse>("GET", "/ui/bundle", req, init);
}

/**
 * Make a conversation's UI build, named by the hash ui_bundle answered, the app's UI: the open app checks it and reloads, nothing restarts. Always asks the operator, since the app runs it with their full access; refused if the build changed since. ui_bundle_rollback puts the previous UI back.
 * POST /ui/bundle/install · risk: destructive
 */
export type UiBundleInstallRequest = InstallReq2;
export type UiBundleInstallResponse = Installed;
export function uiBundleInstall(req: UiBundleInstallRequest, init?: RequestInit): Promise<UiBundleInstallResponse> {
  return request<UiBundleInstallResponse>("POST", "/ui/bundle/install", req, init);
}

/**
 * Put back the app's UI from before the last ui_bundle_install, say when a screenshot shows the new one broken; the open app checks it and reloads. One way only: rolling forward again takes a new install and its approval.
 * POST /ui/bundle/rollback · risk: write
 */
export type UiBundleRollbackRequest = Record<string, never>;
export type UiBundleRollbackResponse = RolledBack;
export function uiBundleRollback(req: UiBundleRollbackRequest = {}, init?: RequestInit): Promise<UiBundleRollbackResponse> {
  return request<UiBundleRollbackResponse>("POST", "/ui/bundle/rollback", req, init);
}

/**
 * Pair a phone or a node with the one-time code Bakery shows; answers the device's own token. The tailnet's only route without a token.
 * POST /pair · operator only
 */
export type PairRequest = PairReq;
export type PairResponse = PairResp;
export function pair(req: PairRequest, init?: RequestInit): Promise<PairResponse> {
  return request<PairResponse>("POST", "/pair", req, init);
}

/**
 * The paired devices, and whether this node is reachable over the tailnet (if not, what to turn on).
 * GET /devices · operator only
 */
export type DevicesRequest = Record<string, never>;
export type DevicesResponse = Devices;
export function devices(req: DevicesRequest = {}, init?: RequestInit): Promise<DevicesResponse> {
  return request<DevicesResponse>("GET", "/devices", req, init);
}

/**
 * Show a one-time pairing code: 8 characters, 5 minutes, one use.
 * POST /devices/pair-code · operator only
 */
export type DevicesPairCodeRequest = Record<string, never>;
export type DevicesPairCodeResponse = PairCode;
export function devicesPairCode(req: DevicesPairCodeRequest = {}, init?: RequestInit): Promise<DevicesPairCodeResponse> {
  return request<DevicesPairCodeResponse>("POST", "/devices/pair-code", req, init);
}

/**
 * Unpair a device: its token stops working and its connections close at once.
 * POST /devices/revoke · operator only
 */
export type DevicesRevokeRequest = RevokeReq;
export type DevicesRevokeResponse = Devices;
export function devicesRevoke(req: DevicesRevokeRequest, init?: RequestInit): Promise<DevicesRevokeResponse> {
  return request<DevicesRevokeResponse>("POST", "/devices/revoke", req, init);
}

/**
 * Make this node reachable from the operator's other devices over Tailscale, or not.
 * POST /devices/remote · operator only
 */
export type DevicesRemoteRequest = RemoteReq;
export type DevicesRemoteResponse = RemoteStatus;
export function devicesRemote(req: DevicesRemoteRequest, init?: RequestInit): Promise<DevicesRemoteResponse> {
  return request<DevicesRemoteResponse>("POST", "/devices/remote", req, init);
}

/**
 * The outside window's read-only token: when it was made and last used, where it is reached and what it may call.
 * GET /outside-window · operator only
 */
export type OutsideWindowRequest = Record<string, never>;
export type OutsideWindowResponse = OutsideWindow;
export function outsideWindow(req: OutsideWindowRequest = {}, init?: RequestInit): Promise<OutsideWindowResponse> {
  return request<OutsideWindowResponse>("GET", "/outside-window", req, init);
}

/**
 * Make the outside window's read-only token, shown this once. One at a time.
 * POST /outside-window/create · operator only
 */
export type OutsideWindowCreateRequest = Record<string, never>;
export type OutsideWindowCreateResponse = Created;
export function outsideWindowCreate(req: OutsideWindowCreateRequest = {}, init?: RequestInit): Promise<OutsideWindowCreateResponse> {
  return request<OutsideWindowCreateResponse>("POST", "/outside-window/create", req, init);
}

/**
 * Revoke the outside window's read-only token: it stops working at once.
 * POST /outside-window/revoke · operator only
 */
export type OutsideWindowRevokeRequest = Record<string, never>;
export type OutsideWindowRevokeResponse = OutsideWindow;
export function outsideWindowRevoke(req: OutsideWindowRevokeRequest = {}, init?: RequestInit): Promise<OutsideWindowRevokeResponse> {
  return request<OutsideWindowRevokeResponse>("POST", "/outside-window/revoke", req, init);
}

/**
 * What your conversation's open preview shows: its URL, title and visible text.
 * POST /preview/snapshot · risk: read
 */
export type PreviewSnapshotRequest = PreviewReq2;
export type PreviewSnapshotResponse = PreviewSnapshot;
export function previewSnapshot(req: PreviewSnapshotRequest, init?: RequestInit): Promise<PreviewSnapshotResponse> {
  return request<PreviewSnapshotResponse>("POST", "/preview/snapshot", req, init);
}

/**
 * Load an http(s) URL in your conversation's open preview.
 * POST /preview/navigate · risk: run
 */
export type PreviewNavigateRequest = PreviewNavigateReq;
export type PreviewNavigateResponse = PreviewPage;
export function previewNavigate(req: PreviewNavigateRequest, init?: RequestInit): Promise<PreviewNavigateResponse> {
  return request<PreviewNavigateResponse>("POST", "/preview/navigate", req, init);
}

/**
 * Click the first element a CSS selector matches in your conversation's open preview.
 * POST /preview/click · risk: run
 */
export type PreviewClickRequest = PreviewClickReq;
export type PreviewClickResponse = PreviewPage;
export function previewClick(req: PreviewClickRequest, init?: RequestInit): Promise<PreviewClickResponse> {
  return request<PreviewClickResponse>("POST", "/preview/click", req, init);
}

/**
 * A screenshot of the Bakery window as the operator sees it, say to check a UI change: you get the image itself beside the result. The app's own page only; a Code or Preview view is never in it. Each capture is noted in your conversation.
 * POST /desktop/screenshot · risk: run
 */
export type DesktopScreenshotRequest = ScreenshotReq;
export type DesktopScreenshotResponse = Screenshot;
export function desktopScreenshot(req: DesktopScreenshotRequest, init?: RequestInit): Promise<DesktopScreenshotResponse> {
  return request<DesktopScreenshotResponse>("POST", "/desktop/screenshot", req, init);
}

/**
 * Run a JavaScript expression in your conversation's open preview and get its value as JSON. It runs in that page as the operator left it, logged-in sessions included.
 * POST /preview/eval · risk: run
 */
export type PreviewEvalRequest = PreviewEvalReq;
export type PreviewEvalResponse = PreviewValue;
export function previewEval(req: PreviewEvalRequest, init?: RequestInit): Promise<PreviewEvalResponse> {
  return request<PreviewEvalResponse>("POST", "/preview/eval", req, init);
}

/**
 * Whether the Ghostty terminal app is available.
 * GET /system/ghostty · risk: read
 */
export type SystemGhosttyRequest = Record<string, never>;
export type SystemGhosttyResponse = GhosttyAvailable;
export function systemGhostty(req: SystemGhosttyRequest = {}, init?: RequestInit): Promise<SystemGhosttyResponse> {
  return request<SystemGhosttyResponse>("GET", "/system/ghostty", req, init);
}

/**
 * Open Ghostty on the operator's screen in a stream's worktree.
 * POST /system/ghostty/open · risk: run
 */
export type SystemGhosttyOpenRequest = GhosttyOpenReq;
export type SystemGhosttyOpenResponse = GhosttyOpenResp;
export function systemGhosttyOpen(req: SystemGhosttyOpenRequest, init?: RequestInit): Promise<SystemGhosttyOpenResponse> {
  return request<SystemGhosttyOpenResponse>("POST", "/system/ghostty/open", req, init);
}

/**
 * One stream's recorded facts: project, branch, worktree, lifetime.
 * GET /stream/{id} · risk: read
 */
export type StreamRequest = { id: string };
export type StreamResponse = Array<EventShape>;
export function stream(req: StreamRequest, init?: RequestInit): Promise<StreamResponse> {
  const { id, ...rest } = req;
  return request<StreamResponse>("GET", `/stream/${encodeURIComponent(id)}`, rest, init);
}

/**
 * Assign a model id (a model_options id) to a conversation's seat. While the seat's turn runs, the switch waits for that turn to end (a seat.model.pending answer) and the turn runs on; a second switch replaces it, and one back to what the seat has cancels it.
 * POST /seat/assign · risk: write
 */
export type SeatAssignRequest = SeatAssignReq;
export type SeatAssignResponse = EventShape;
export function seatAssign(req: SeatAssignRequest, init?: RequestInit): Promise<SeatAssignResponse> {
  return request<SeatAssignResponse>("POST", "/seat/assign", req, init);
}

/**
 * Decide a native permission request.
 * POST /agent/pipe/gate/decide · operator only
 */
export type AgentPipeGateDecideRequest = ChildToolGateDecideReq;
export type AgentPipeGateDecideResponse = EventShape;
export function agentPipeGateDecide(req: AgentPipeGateDecideRequest, init?: RequestInit): Promise<AgentPipeGateDecideResponse> {
  return request<AgentPipeGateDecideResponse>("POST", "/agent/pipe/gate/decide", req, init);
}

/**
 * Set a stream's lifetime: living or burn.
 * POST /stream/lifetime · risk: write
 */
export type StreamLifetimeRequest = LifetimeReq;
export type StreamLifetimeResponse = LifetimeResp;
export function streamLifetime(req: StreamLifetimeRequest, init?: RequestInit): Promise<StreamLifetimeResponse> {
  return request<StreamLifetimeResponse>("POST", "/stream/lifetime", req, init);
}

/**
 * Throw away a stream's worktree and sandbox room; its history is kept.
 * POST /stream/discard · risk: destructive
 */
export type StreamDiscardRequest = DiscardReq;
export type StreamDiscardResponse = DiscardResp;
export function streamDiscard(req: StreamDiscardRequest, init?: RequestInit): Promise<StreamDiscardResponse> {
  return request<StreamDiscardResponse>("POST", "/stream/discard", req, init);
}

/**
 * One directory level of a stream's worktree.
 * GET /files/list · risk: read
 */
export type FilesListRequest = FilesQuery;
export type FilesListResponse = Array<DirEntryInfo>;
export function filesList(req: FilesListRequest, init?: RequestInit): Promise<FilesListResponse> {
  return request<FilesListResponse>("GET", "/files/list", req, init);
}

/**
 * A file's full text from a stream's worktree, with its digest.
 * GET /files/read · risk: read
 */
export type FilesReadRequest = FilesQuery;
export type FilesReadResponse = FileReadResp;
export function filesRead(req: FilesReadRequest, init?: RequestInit): Promise<FilesReadResponse> {
  return request<FilesReadResponse>("GET", "/files/read", req, init);
}

/**
 * The operator's editor save.
 * POST /files/write · operator only
 */
export type FilesWriteRequest = FileWriteReq;
export type FilesWriteResponse = FileWriteResp;
export function filesWrite(req: FilesWriteRequest, init?: RequestInit): Promise<FilesWriteResponse> {
  return request<FilesWriteResponse>("POST", "/files/write", req, init);
}

/**
 * Run one agent turn synchronously.
 * POST /agent/turn · operator only
 */
export type AgentTurnRequest = AgentTurnReq;
export type AgentTurnResponse = EventShape;
export function agentTurn(req: AgentTurnRequest, init?: RequestInit): Promise<AgentTurnResponse> {
  return request<AgentTurnResponse>("POST", "/agent/turn", req, init);
}

/**
 * Upload an attachment from the native picker.
 * POST /agent/attachments · operator only
 */
export type AgentAttachmentsRequest = AttachmentUploadReq;
export type AgentAttachmentsResponse = AttachmentRef;
export function agentAttachments(req: AgentAttachmentsRequest, init?: RequestInit): Promise<AgentAttachmentsResponse> {
  return request<AgentAttachmentsResponse>("POST", "/agent/attachments", req, init);
}

/**
 * An attachment sent on a thread, as it was sent, with its media type.
 * GET /attachment · operator only
 */
export type AttachmentRequest = AttachmentQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type AttachmentResponse = unknown;
export function attachment(req: AttachmentRequest, init?: RequestInit): Promise<AttachmentResponse> {
  return request<AttachmentResponse>("GET", "/attachment", req, init);
}

/**
 * Queue a message for a conversation's agent; it runs when the agent is free.
 * POST /agent/turn/queue · risk: write
 */
export type AgentTurnQueueRequest = AgentQueueReq;
export type AgentTurnQueueResponse = QueuedShape;
export function agentTurnQueue(req: AgentTurnQueueRequest, init?: RequestInit): Promise<AgentTurnQueueResponse> {
  return request<AgentTurnQueueResponse>("POST", "/agent/turn/queue", req, init);
}

/**
 * Review all the changes one agent turn made at once: keep every one, or undo every one.
 * POST /agent/turn/review · operator only
 */
export type AgentTurnReviewRequest = ReviewTurnReq;
export type AgentTurnReviewResponse = TurnReviewed;
export function agentTurnReview(req: AgentTurnReviewRequest, init?: RequestInit): Promise<AgentTurnReviewResponse> {
  return request<AgentTurnReviewResponse>("POST", "/agent/turn/review", req, init);
}

/**
 * One change an agent's turn made, before and after, read from Bakery's own copy rather than the history.
 * GET /agent/change · operator only
 */
export type AgentChangeRequest = ChangeQuery;
export type AgentChangeResponse = ChangeContent;
export function agentChange(req: AgentChangeRequest, init?: RequestInit): Promise<AgentChangeResponse> {
  return request<AgentChangeResponse>("GET", "/agent/change", req, init);
}

/**
 * The largest file, in MB, kept a copy of when a turn's changes are recorded: a bigger file's change can only be kept, never undone. 10 MB until the operator sets it.
 * GET /agent/snapshot-limit · operator only
 */
export type AgentSnapshotLimitRequest = Record<string, never>;
export type AgentSnapshotLimitResponse = SnapshotLimit;
export function agentSnapshotLimit(req: AgentSnapshotLimitRequest = {}, init?: RequestInit): Promise<AgentSnapshotLimitResponse> {
  return request<AgentSnapshotLimitResponse>("GET", "/agent/snapshot-limit", req, init);
}

/**
 * Set that size limit, in MB (at least 1). A turn already running keeps the limit it began with.
 * POST /agent/snapshot-limit · operator only
 */
export type AgentSnapshotLimitSetRequest = SnapshotLimit;
export type AgentSnapshotLimitSetResponse = SnapshotLimit;
export function agentSnapshotLimitSet(req: AgentSnapshotLimitSetRequest, init?: RequestInit): Promise<AgentSnapshotLimitSetResponse> {
  return request<AgentSnapshotLimitSetResponse>("POST", "/agent/snapshot-limit", req, init);
}

/**
 * The operator's own search: file names and text in this workspace or everywhere the Bakery has a folder, secret files included with their values masked. Folders git ignores are skipped. Never recorded.
 * POST /search · operator only
 */
export type SearchRequest = SearchReq;
export type SearchResponse = Searched;
export function search(req: SearchRequest, init?: RequestInit): Promise<SearchResponse> {
  return request<SearchResponse>("POST", "/search", req, init);
}

/**
 * One masked line from a secret file, shown whole, for the operator alone. Never recorded.
 * POST /search/reveal · operator only
 */
export type SearchRevealRequest = RevealReq;
export type SearchRevealResponse = Revealed;
export function searchReveal(req: SearchRevealRequest, init?: RequestInit): Promise<SearchRevealResponse> {
  return request<SearchRevealResponse>("POST", "/search/reveal", req, init);
}

/**
 * Turn every queued follow-up into one steer for the agent's running turn.
 * POST /agent/turn/queue/steer-all · risk: write
 */
export type AgentTurnQueueSteerAllRequest = AgentTurnCancelReq;
export type AgentTurnQueueSteerAllResponse = EventShape;
export function agentTurnQueueSteerAll(req: AgentTurnQueueSteerAllRequest, init?: RequestInit): Promise<AgentTurnQueueSteerAllResponse> {
  return request<AgentTurnQueueSteerAllResponse>("POST", "/agent/turn/queue/steer-all", req, init);
}

/**
 * Take back a message still waiting in an agent's queue, a follow-up or a steer not yet delivered, to edit or drop it; its text comes back. Refused once it is sent, merged, or on its way into the running turn.
 * POST /agent/turn/queue/withdraw · operator only
 */
export type AgentTurnQueueWithdrawRequest = WithdrawReq;
export type AgentTurnQueueWithdrawResponse = Withdrawn;
export function agentTurnQueueWithdraw(req: AgentTurnQueueWithdrawRequest, init?: RequestInit): Promise<AgentTurnQueueWithdrawResponse> {
  return request<AgentTurnQueueWithdrawResponse>("POST", "/agent/turn/queue/withdraw", req, init);
}

/**
 * The complete payload behind one native tool call record.
 * GET /agent/tool-payload · risk: read
 */
export type AgentToolPayloadRequest = ToolPayloadQuery;
/** Untyped JSON: no schema on the node describes it yet. */
export type AgentToolPayloadResponse = unknown;
export function agentToolPayload(req: AgentToolPayloadRequest, init?: RequestInit): Promise<AgentToolPayloadResponse> {
  return request<AgentToolPayloadResponse>("GET", "/agent/tool-payload", req, init);
}

/**
 * Steer a conversation's running turn with a message.
 * POST /agent/turn/steer · risk: write
 */
export type AgentTurnSteerRequest = AgentQueueReq;
export type AgentTurnSteerResponse = EventShape;
export function agentTurnSteer(req: AgentTurnSteerRequest, init?: RequestInit): Promise<AgentTurnSteerResponse> {
  return request<AgentTurnSteerResponse>("POST", "/agent/turn/steer", req, init);
}

/**
 * Stop a conversation's running turn and pause its queue (queued messages are kept).
 * POST /agent/turn/cancel · risk: write
 */
export type TurnStopRequest = AgentTurnCancelReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type TurnStopResponse = unknown;
export function turnStop(req: TurnStopRequest, init?: RequestInit): Promise<TurnStopResponse> {
  return request<TurnStopResponse>("POST", "/agent/turn/cancel", req, init);
}

/**
 * Resume a paused conversation queue.
 * POST /agent/dispatch/resume · risk: write
 */
export type AgentDispatchResumeRequest = AgentTurnCancelReq;
export type AgentDispatchResumeResponse = EventShape;
export function agentDispatchResume(req: AgentDispatchResumeRequest, init?: RequestInit): Promise<AgentDispatchResumeResponse> {
  return request<AgentDispatchResumeResponse>("POST", "/agent/dispatch/resume", req, init);
}

/**
 * Set a conversation's operating mode: manual, plan, auto_edit or auto.
 * POST /agent/mode · risk: write
 */
export type AgentModeRequest = AgentModeReq;
export type AgentModeResponse = EventShape;
export function agentMode(req: AgentModeRequest, init?: RequestInit): Promise<AgentModeResponse> {
  return request<AgentModeResponse>("POST", "/agent/mode", req, init);
}

/**
 * What a conversation's agents may use of the Bakery's tools: all, read (only those graded read) or none.
 * GET /agent/tools · risk: read
 */
export type AgentToolsRequest = ToolsQuery;
export type AgentToolsResponse = Tools;
export function agentTools(req: AgentToolsRequest, init?: RequestInit): Promise<AgentToolsResponse> {
  return request<AgentToolsResponse>("GET", "/agent/tools", req, init);
}

/**
 * Set what a conversation's agents may use of the Bakery's tools: all, read or none.
 * POST /agent/tools · operator only
 */
export type AgentToolsSetRequest = ToolsReq;
export type AgentToolsSetResponse = Tools;
export function agentToolsSet(req: AgentToolsSetRequest, init?: RequestInit): Promise<AgentToolsSetResponse> {
  return request<AgentToolsSetResponse>("POST", "/agent/tools", req, init);
}

/**
 * Set one native runtime option (model, effort, ...) a session advertises.
 * POST /agent/runtime/config · risk: write
 */
export type AgentRuntimeConfigRequest = NativeConfigReq;
export type AgentRuntimeConfigResponse = AcpRuntimeSnapshot;
export function agentRuntimeConfig(req: AgentRuntimeConfigRequest, init?: RequestInit): Promise<AgentRuntimeConfigResponse> {
  return request<AgentRuntimeConfigResponse>("POST", "/agent/runtime/config", req, init);
}

/**
 * Set the model and effort a conversation on the Bakery's own loop runs every turn on, as its picker shows them.
 * POST /agent/selection · risk: write
 */
export type AgentSelectionRequest = SelectionReq;
export type AgentSelectionResponse = Selection;
export function agentSelection(req: AgentSelectionRequest, init?: RequestInit): Promise<AgentSelectionResponse> {
  return request<AgentSelectionResponse>("POST", "/agent/selection", req, init);
}

/**
 * What a conversation's agent is doing right now: phase, running tool, finished tools.
 * GET /agent/activity · risk: read
 */
export type AgentActivityRequest = AgentActivityQuery;
export type AgentActivityResponse = AgentActivity | null;
export function agentActivity(req: AgentActivityRequest, init?: RequestInit): Promise<AgentActivityResponse> {
  return request<AgentActivityResponse>("GET", "/agent/activity", req, init);
}

/**
 * Accept a write proposal.
 * POST /agent/accept · operator only
 */
export type AgentAcceptRequest = AgentAcceptReq;
export type AgentAcceptResponse = EventShape;
export function agentAccept(req: AgentAcceptRequest, init?: RequestInit): Promise<AgentAcceptResponse> {
  return request<AgentAcceptResponse>("POST", "/agent/accept", req, init);
}

/**
 * Answer an agent's question.
 * POST /agent/answer · operator only
 */
export type AgentAnswerRequest = AgentAnswerReq;
export type AgentAnswerResponse = EventShape;
export function agentAnswer(req: AgentAnswerRequest, init?: RequestInit): Promise<AgentAnswerResponse> {
  return request<AgentAnswerResponse>("POST", "/agent/answer", req, init);
}

/**
 * Resume a turn that died with the process.
 * POST /agent/resume · operator only
 */
export type AgentResumeRequest = AgentResumeReq;
export type AgentResumeResponse = EventShape;
export function agentResume(req: AgentResumeRequest, init?: RequestInit): Promise<AgentResumeResponse> {
  return request<AgentResumeResponse>("POST", "/agent/resume", req, init);
}

/**
 * Answer a session that didn't resume: retry it, or start fresh.
 * POST /agent/session/choose · operator only
 */
export type AgentSessionChooseRequest = SessionChoiceReq;
export type AgentSessionChooseResponse = EventShape;
export function agentSessionChoose(req: AgentSessionChooseRequest, init?: RequestInit): Promise<AgentSessionChooseResponse> {
  return request<AgentSessionChooseResponse>("POST", "/agent/session/choose", req, init);
}

/**
 * Reject a write proposal.
 * POST /agent/reject · operator only
 */
export type AgentRejectRequest = AgentDecisionReq;
export type AgentRejectResponse = EventShape;
export function agentReject(req: AgentRejectRequest, init?: RequestInit): Promise<AgentRejectResponse> {
  return request<AgentRejectResponse>("POST", "/agent/reject", req, init);
}

/**
 * The permanent Baker conversation.
 * GET /baker · risk: read
 */
export type BakerRequest = Record<string, never>;
export type BakerResponse = BakerHome;
export function baker(req: BakerRequest = {}, init?: RequestInit): Promise<BakerResponse> {
  return request<BakerResponse>("GET", "/baker", req, init);
}

/**
 * The Baker's conversations: the main one and every parallel thread.
 * GET /baker/threads · risk: read
 */
export type ThreadListRequest = ListThreadsQuery;
export type ThreadListResponse = Array<BakerThreadView>;
export function threadList(req: ThreadListRequest, init?: RequestInit): Promise<ThreadListResponse> {
  return request<ThreadListResponse>("GET", "/baker/threads", req, init);
}

/**
 * Open a new Baker thread; for another agent, a peer of its own (a workspace's orc: a worker).
 * POST /baker/threads · risk: write
 */
export type ThreadCreateRequest = CreateThreadReq;
export type ThreadCreateResponse = CreateThreadResp;
export function threadCreate(req: ThreadCreateRequest, init?: RequestInit): Promise<ThreadCreateResponse> {
  return request<ThreadCreateResponse>("POST", "/baker/threads", req, init);
}

/**
 * Rename a Baker thread.
 * POST /baker/threads/rename · risk: write
 */
export type ThreadRenameRequest = RenameThreadReq;
export type ThreadRenameResponse = BakerThreadView;
export function threadRename(req: ThreadRenameRequest, init?: RequestInit): Promise<ThreadRenameResponse> {
  return request<ThreadRenameResponse>("POST", "/baker/threads/rename", req, init);
}

/**
 * Archive or restore a Baker thread.
 * POST /baker/threads/archive · risk: write
 */
export type ThreadArchiveRequest = ArchiveThreadReq;
export type ThreadArchiveResponse = BakerThreadView;
export function threadArchive(req: ThreadArchiveRequest, init?: RequestInit): Promise<ThreadArchiveResponse> {
  return request<ThreadArchiveResponse>("POST", "/baker/threads/archive", req, init);
}

/**
 * A conversation's work: its runtime, model, effort and mode, its latest messages, and its last finished turn's result (status, stop reason, tokens, acceptance).
 * GET /baker/threads/read · risk: read
 */
export type ThreadReadRequest = ThreadReadQuery;
export type ThreadReadResponse = ThreadRead;
export function threadRead(req: ThreadReadRequest, init?: RequestInit): Promise<ThreadReadResponse> {
  return request<ThreadReadResponse>("GET", "/baker/threads/read", req, init);
}

/**
 * Wait until a conversation has nothing left to run, up to 50 s, then return what thread_read returns, plus done:false when it is still working.
 * POST /baker/threads/wait · risk: read
 */
export type ThreadWaitRequest = ThreadWaitReq;
export type ThreadWaitResponse = ThreadWaited;
export function threadWait(req: ThreadWaitRequest, init?: RequestInit): Promise<ThreadWaitResponse> {
  return request<ThreadWaitResponse>("POST", "/baker/threads/wait", req, init);
}

/**
 * Every thread you look after, in one call (an orc: itself and its workers; the Baker: everything, its conversations first, then every workspace, worker and project thread in use; another agent: itself and the peers it opened): each one's state and last line, its branch, and from git, against main (a workspace's worker against its workspace's branch too), the commits not in yet, whether they are merged or it made none, and whether the rest merges cleanly.
 * GET /baker/threads/board · risk: read
 */
export type ThreadBoardRequest = BoardQuery;
export type ThreadBoardResponse = Board;
export function threadBoard(req: ThreadBoardRequest, init?: RequestInit): Promise<ThreadBoardResponse> {
  return request<ThreadBoardResponse>("GET", "/baker/threads/board", req, init);
}

/**
 * Start a legacy Baker branch job on a repository.
 * POST /baker/assign · risk: run
 */
export type BakerAssignRequest = BakerAssignReq;
export type BakerAssignResponse = BakerAssignResp;
export function bakerAssign(req: BakerAssignRequest, init?: RequestInit): Promise<BakerAssignResponse> {
  return request<BakerAssignResponse>("POST", "/baker/assign", req, init);
}

/**
 * Legacy Baker branch jobs.
 * GET /baker/work · risk: read
 */
export type BakerWorkRequest = Record<string, never>;
export type BakerWorkResponse = BakerWorkList;
export function bakerWork(req: BakerWorkRequest = {}, init?: RequestInit): Promise<BakerWorkResponse> {
  return request<BakerWorkResponse>("GET", "/baker/work", req, init);
}

/**
 * One legacy Baker branch job.
 * GET /baker/work/detail · risk: read
 */
export type BakerWorkDetailRequest = BakerWorkDetailQuery;
export type BakerWorkDetailResponse = BakerWorkDetail;
export function bakerWorkDetail(req: BakerWorkDetailRequest, init?: RequestInit): Promise<BakerWorkDetailResponse> {
  return request<BakerWorkDetailResponse>("GET", "/baker/work/detail", req, init);
}

/**
 * Push a Baker branch to its remote.
 * POST /baker/push · risk: destructive
 */
export type BakerPushRequest = BakerBranchReq;
export type BakerPushResponse = BakerActionResp;
export function bakerPush(req: BakerPushRequest, init?: RequestInit): Promise<BakerPushResponse> {
  return request<BakerPushResponse>("POST", "/baker/push", req, init);
}

/**
 * Open a pull request for a Baker branch.
 * POST /baker/pr · risk: destructive
 */
export type BakerPrRequest = BakerPrReq;
export type BakerPrResponse = BakerActionResp;
export function bakerPr(req: BakerPrRequest, init?: RequestInit): Promise<BakerPrResponse> {
  return request<BakerPrResponse>("POST", "/baker/pr", req, init);
}

/**
 * Merge a Baker branch.
 * POST /baker/merge · risk: destructive
 */
export type BakerMergeRequest = BakerMergeReq;
export type BakerMergeResponse = BakerActionResp;
export function bakerMerge(req: BakerMergeRequest, init?: RequestInit): Promise<BakerMergeResponse> {
  return request<BakerMergeResponse>("POST", "/baker/merge", req, init);
}

/**
 * What the Baker can do: its operations and their summaries.
 * GET /baker/capabilities · risk: read
 */
export type BakerCapabilitiesRequest = CapabilitiesQuery;
export type BakerCapabilitiesResponse = CapabilitiesView;
export function bakerCapabilities(req: BakerCapabilitiesRequest, init?: RequestInit): Promise<BakerCapabilitiesResponse> {
  return request<BakerCapabilitiesResponse>("GET", "/baker/capabilities", req, init);
}

/**
 * Create a task: its durable identity and completion contract.
 * POST /task/create · risk: write
 */
export type TaskCreateRequest = TaskCreateReq;
export type TaskCreateResponse = TaskCreateResp;
export function taskCreate(req: TaskCreateRequest, init?: RequestInit): Promise<TaskCreateResponse> {
  return request<TaskCreateResponse>("POST", "/task/create", req, init);
}

/**
 * Materialize a task's workspace and start its agent.
 * POST /task/start · risk: run
 */
export type TaskStartRequest = TaskStartReq;
export type TaskStartResponse = TaskStartResp;
export function taskStart(req: TaskStartRequest, init?: RequestInit): Promise<TaskStartResponse> {
  return request<TaskStartResponse>("POST", "/task/start", req, init);
}

/**
 * Queue a steering message in a task's inbox.
 * POST /task/message · risk: write
 */
export type TaskMessageRequest = TaskMessageReq;
export type TaskMessageResponse = TaskMessageResp;
export function taskMessage(req: TaskMessageRequest, init?: RequestInit): Promise<TaskMessageResponse> {
  return request<TaskMessageResponse>("POST", "/task/message", req, init);
}

/**
 * Cancel a task.
 * POST /task/cancel · risk: write
 */
export type TaskCancelRequest = TaskCancelReq;
export type TaskCancelResponse = TaskCancelResp;
export function taskCancel(req: TaskCancelRequest, init?: RequestInit): Promise<TaskCancelResponse> {
  return request<TaskCancelResponse>("POST", "/task/cancel", req, init);
}

/**
 * Retire a task and release its worktree and sandbox room.
 * POST /task/retire · risk: destructive
 */
export type TaskRetireRequest = TaskRetireReq;
export type TaskRetireResponse = TaskRetireResp;
export function taskRetire(req: TaskRetireRequest, init?: RequestInit): Promise<TaskRetireResponse> {
  return request<TaskRetireResponse>("POST", "/task/retire", req, init);
}

/**
 * The tasks on one Project: status, contract, warnings.
 * GET /task/list · risk: read
 */
export type TaskListRequest = TaskListQuery;
export type TaskListResponse = Array<TaskView>;
export function taskList(req: TaskListRequest, init?: RequestInit): Promise<TaskListResponse> {
  return request<TaskListResponse>("GET", "/task/list", req, init);
}

/**
 * A task's review: its diff against the baseline and its checks.
 * GET /task/review · risk: read
 */
export type TaskReviewRequest = TaskReviewQuery;
export type TaskReviewResponse = TaskReviewResp;
export function taskReview(req: TaskReviewRequest, init?: RequestInit): Promise<TaskReviewResponse> {
  return request<TaskReviewResponse>("GET", "/task/review", req, init);
}

/**
 * Sign off a task's review.
 * POST /task/signoff · operator only
 */
export type TaskSignoffRequest = TaskSignoffReq;
export type TaskSignoffResponse = TaskSignoffResp;
export function taskSignoff(req: TaskSignoffRequest, init?: RequestInit): Promise<TaskSignoffResponse> {
  return request<TaskSignoffResponse>("POST", "/task/signoff", req, init);
}

/**
 * Push a task's branch to its remote.
 * POST /task/push · risk: destructive
 */
export type TaskPushRequest = TaskShipReq;
export type TaskPushResponse = TaskShipResp;
export function taskPush(req: TaskPushRequest, init?: RequestInit): Promise<TaskPushResponse> {
  return request<TaskPushResponse>("POST", "/task/push", req, init);
}

/**
 * Open a pull request for a task's branch.
 * POST /task/pr · risk: destructive
 */
export type TaskPrRequest = TaskShipReq;
export type TaskPrResponse = TaskShipResp;
export function taskPr(req: TaskPrRequest, init?: RequestInit): Promise<TaskPrResponse> {
  return request<TaskPrResponse>("POST", "/task/pr", req, init);
}

/**
 * Merge a signed-off task into its project's branch.
 * POST /task/integrate · risk: destructive
 */
export type TaskIntegrateRequest = TaskShipReq;
export type TaskIntegrateResponse = TaskShipResp;
export function taskIntegrate(req: TaskIntegrateRequest, init?: RequestInit): Promise<TaskIntegrateResponse> {
  return request<TaskIntegrateResponse>("POST", "/task/integrate", req, init);
}

/**
 * One snapshot of the node: streams, gates, agents.
 * GET /operator/snapshot · risk: read
 */
export type OperatorSnapshotRequest = Record<string, never>;
export type OperatorSnapshotResponse = OperatorSnapshot;
export function operatorSnapshot(req: OperatorSnapshotRequest = {}, init?: RequestInit): Promise<OperatorSnapshotResponse> {
  return request<OperatorSnapshotResponse>("GET", "/operator/snapshot", req, init);
}

/**
 * Provider credential status.
 * GET /auth/status · operator only
 */
export type AuthStatusRequest = Record<string, never>;
export type AuthStatusResponse = AuthStatus;
export function authStatus(req: AuthStatusRequest = {}, init?: RequestInit): Promise<AuthStatusResponse> {
  return request<AuthStatusResponse>("GET", "/auth/status", req, init);
}

/**
 * CLI login status.
 * GET /auth/cli-status · operator only
 */
export type AuthCliStatusRequest = CliStatusQuery;
export type AuthCliStatusResponse = CliAccountsStatus;
export function authCliStatus(req: AuthCliStatusRequest, init?: RequestInit): Promise<AuthCliStatusResponse> {
  return request<AuthCliStatusResponse>("GET", "/auth/cli-status", req, init);
}

/**
 * Claude login status.
 * GET /auth/claude-native/status · operator only
 */
export type AuthClaudeNativeStatusRequest = Record<string, never>;
export type AuthClaudeNativeStatusResponse = ClaudeNativeStatus;
export function authClaudeNativeStatus(req: AuthClaudeNativeStatusRequest = {}, init?: RequestInit): Promise<AuthClaudeNativeStatusResponse> {
  return request<AuthClaudeNativeStatusResponse>("GET", "/auth/claude-native/status", req, init);
}

/**
 * Set an API key.
 * POST /auth/api-key · operator only
 */
export type AuthApiKeyRequest = ApiKeyReq;
export type AuthApiKeyResponse = AuthStatus;
export function authApiKey(req: AuthApiKeyRequest, init?: RequestInit): Promise<AuthApiKeyResponse> {
  return request<AuthApiKeyResponse>("POST", "/auth/api-key", req, init);
}

/**
 * Clear a credential.
 * POST /auth/clear · operator only
 */
export type AuthClearRequest = ClearReq;
export type AuthClearResponse = AuthStatus;
export function authClear(req: AuthClearRequest, init?: RequestInit): Promise<AuthClearResponse> {
  return request<AuthClearResponse>("POST", "/auth/clear", req, init);
}

/**
 * Set a provider model.
 * POST /auth/model · operator only
 */
export type AuthModelRequest = ModelReq;
export type AuthModelResponse = AuthStatus;
export function authModel(req: AuthModelRequest, init?: RequestInit): Promise<AuthModelResponse> {
  return request<AuthModelResponse>("POST", "/auth/model", req, init);
}

/**
 * Start an xAI device login.
 * POST /auth/xai/device/start · operator only
 */
export type AuthXaiDeviceStartRequest = Record<string, never>;
export type AuthXaiDeviceStartResponse = DeviceStart;
export function authXaiDeviceStart(req: AuthXaiDeviceStartRequest = {}, init?: RequestInit): Promise<AuthXaiDeviceStartResponse> {
  return request<AuthXaiDeviceStartResponse>("POST", "/auth/xai/device/start", req, init);
}

/**
 * Poll an xAI device login.
 * POST /auth/xai/device/poll · operator only
 */
export type AuthXaiDevicePollRequest = DevicePollReq;
export type AuthXaiDevicePollResponse = DevicePollResult;
export function authXaiDevicePoll(req: AuthXaiDevicePollRequest, init?: RequestInit): Promise<AuthXaiDevicePollResponse> {
  return request<AuthXaiDevicePollResponse>("POST", "/auth/xai/device/poll", req, init);
}

/**
 * Import the Gemini CLI login.
 * POST /auth/gemini/import-cli · operator only
 */
export type AuthGeminiImportCliRequest = Record<string, never>;
export type AuthGeminiImportCliResponse = ImportResp;
export function authGeminiImportCli(req: AuthGeminiImportCliRequest = {}, init?: RequestInit): Promise<AuthGeminiImportCliResponse> {
  return request<AuthGeminiImportCliResponse>("POST", "/auth/gemini/import-cli", req, init);
}

/**
 * Start a Claude Code login.
 * POST /auth/claude-code/login/start · operator only
 */
export type AuthClaudeCodeLoginStartRequest = Record<string, never>;
export type AuthClaudeCodeLoginStartResponse = LoginStart;
export function authClaudeCodeLoginStart(req: AuthClaudeCodeLoginStartRequest = {}, init?: RequestInit): Promise<AuthClaudeCodeLoginStartResponse> {
  return request<AuthClaudeCodeLoginStartResponse>("POST", "/auth/claude-code/login/start", req, init);
}

/**
 * Complete a Claude Code login.
 * POST /auth/claude-code/login/complete · operator only
 */
export type AuthClaudeCodeLoginCompleteRequest = ClaudeCodeCompleteReq;
export type AuthClaudeCodeLoginCompleteResponse = LoginResult;
export function authClaudeCodeLoginComplete(req: AuthClaudeCodeLoginCompleteRequest, init?: RequestInit): Promise<AuthClaudeCodeLoginCompleteResponse> {
  return request<AuthClaudeCodeLoginCompleteResponse>("POST", "/auth/claude-code/login/complete", req, init);
}

/**
 * Set a plug secret.
 * POST /auth/plug-secret · operator only
 */
export type AuthPlugSecretRequest = PlugSecretReq;
export type AuthPlugSecretResponse = PlugSecretStatus;
export function authPlugSecret(req: AuthPlugSecretRequest, init?: RequestInit): Promise<AuthPlugSecretResponse> {
  return request<AuthPlugSecretResponse>("POST", "/auth/plug-secret", req, init);
}

/**
 * Clear a plug secret.
 * POST /auth/plug-secret/clear · operator only
 */
export type AuthPlugSecretClearRequest = PlugSecretClearReq;
export type AuthPlugSecretClearResponse = PlugSecretStatus;
export function authPlugSecretClear(req: AuthPlugSecretClearRequest, init?: RequestInit): Promise<AuthPlugSecretClearResponse> {
  return request<AuthPlugSecretClearResponse>("POST", "/auth/plug-secret/clear", req, init);
}

/**
 * The configured model providers and runtime plugs.
 * GET /providers · risk: read
 */
export type ProvidersRequest = Record<string, never>;
export type ProvidersResponse = PlugListResp;
export function providers(req: ProvidersRequest = {}, init?: RequestInit): Promise<ProvidersResponse> {
  return request<ProvidersResponse>("GET", "/providers", req, init);
}

/**
 * Add an ACP runtime plug.
 * POST /providers/acp-plug · operator only
 */
export type ProvidersAcpPlugRequest = AcpPlugCreateReq;
export type ProvidersAcpPlugResponse = AcpPlugConfig;
export function providersAcpPlug(req: ProvidersAcpPlugRequest, init?: RequestInit): Promise<ProvidersAcpPlugResponse> {
  return request<ProvidersAcpPlugResponse>("POST", "/providers/acp-plug", req, init);
}

/**
 * Change an ACP runtime plug.
 * POST /providers/acp-plug/update · operator only
 */
export type ProvidersAcpPlugUpdateRequest = AcpPlugUpdateReq;
export type ProvidersAcpPlugUpdateResponse = AcpPlugConfig;
export function providersAcpPlugUpdate(req: ProvidersAcpPlugUpdateRequest, init?: RequestInit): Promise<ProvidersAcpPlugUpdateResponse> {
  return request<ProvidersAcpPlugUpdateResponse>("POST", "/providers/acp-plug/update", req, init);
}

/**
 * Remove an ACP runtime plug.
 * POST /providers/acp-plug/delete · operator only
 */
export type ProvidersAcpPlugDeleteRequest = PlugIdReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProvidersAcpPlugDeleteResponse = unknown;
export function providersAcpPlugDelete(req: ProvidersAcpPlugDeleteRequest, init?: RequestInit): Promise<ProvidersAcpPlugDeleteResponse> {
  return request<ProvidersAcpPlugDeleteResponse>("POST", "/providers/acp-plug/delete", req, init);
}

/**
 * Add an OpenAI-compatible plug.
 * POST /providers/openai-compat-plug · operator only
 */
export type ProvidersOpenaiCompatPlugRequest = OpenAiCompatPlugCreateReq;
export type ProvidersOpenaiCompatPlugResponse = OpenAiCompatPlugConfig;
export function providersOpenaiCompatPlug(req: ProvidersOpenaiCompatPlugRequest, init?: RequestInit): Promise<ProvidersOpenaiCompatPlugResponse> {
  return request<ProvidersOpenaiCompatPlugResponse>("POST", "/providers/openai-compat-plug", req, init);
}

/**
 * Change an OpenAI-compatible plug.
 * POST /providers/openai-compat-plug/update · operator only
 */
export type ProvidersOpenaiCompatPlugUpdateRequest = OpenAiCompatPlugUpdateReq;
export type ProvidersOpenaiCompatPlugUpdateResponse = OpenAiCompatPlugConfig;
export function providersOpenaiCompatPlugUpdate(req: ProvidersOpenaiCompatPlugUpdateRequest, init?: RequestInit): Promise<ProvidersOpenaiCompatPlugUpdateResponse> {
  return request<ProvidersOpenaiCompatPlugUpdateResponse>("POST", "/providers/openai-compat-plug/update", req, init);
}

/**
 * Remove an OpenAI-compatible plug.
 * POST /providers/openai-compat-plug/delete · operator only
 */
export type ProvidersOpenaiCompatPlugDeleteRequest = PlugIdReq;
/** Untyped JSON: no schema on the node describes it yet. */
export type ProvidersOpenaiCompatPlugDeleteResponse = unknown;
export function providersOpenaiCompatPlugDelete(req: ProvidersOpenaiCompatPlugDeleteRequest, init?: RequestInit): Promise<ProvidersOpenaiCompatPlugDeleteResponse> {
  return request<ProvidersOpenaiCompatPlugDeleteResponse>("POST", "/providers/openai-compat-plug/delete", req, init);
}

/**
 * The studio's whole state: projects, conversations, seats, models.
 * GET /studio/state · risk: read
 */
export type StudioStateRequest = Record<string, never>;
export type StudioStateResponse = StudioState;
export function studioState(req: StudioStateRequest = {}, init?: RequestInit): Promise<StudioStateResponse> {
  return request<StudioStateResponse>("GET", "/studio/state", req, init);
}

/**
 * One provider's live models with their effort levels and context windows.
 * GET /models/catalog · risk: read
 */
export type ModelsCatalogRequest = ModelCatalogQuery;
export type ModelsCatalogResponse = ModelCatalog;
export function modelsCatalog(req: ModelsCatalogRequest, init?: RequestInit): Promise<ModelsCatalogResponse> {
  return request<ModelsCatalogResponse>("GET", "/models/catalog", req, init);
}

/**
 * What one account's runtime can do: live catalog, settings, auth.
 * GET /accounts/capabilities · risk: read
 */
export type AccountsCapabilitiesRequest = AccountCapabilitiesQuery;
export type AccountsCapabilitiesResponse = AccountCapabilities;
export function accountsCapabilities(req: AccountsCapabilitiesRequest, init?: RequestInit): Promise<AccountsCapabilitiesResponse> {
  return request<AccountsCapabilitiesResponse>("GET", "/accounts/capabilities", req, init);
}

/**
 * MCP tool list.
 * GET /mcp/tools · operator only
 */
export type McpToolsRequest = Record<string, never>;
export type McpToolsResponse = Array<McpToolWire>;
export function mcpTools(req: McpToolsRequest = {}, init?: RequestInit): Promise<McpToolsResponse> {
  return request<McpToolsResponse>("GET", "/mcp/tools", req, init);
}

/**
 * MCP tool call.
 * POST /mcp/call · operator only
 */
export type McpCallRequest = McpCallReq;
export type McpCallResponse = McpCallResp;
export function mcpCall(req: McpCallRequest, init?: RequestInit): Promise<McpCallResponse> {
  return request<McpCallResponse>("POST", "/mcp/call", req, init);
}

/**
 * The outside window: read-only MCP over HTTP, for its read-only token.
 * POST /mcp · operator only
 */
export type McpRequest = Record<string, never>;
/** Untyped JSON: no schema on the node describes it yet. */
export type McpResponse = unknown;
export function mcp(req: McpRequest = {}, init?: RequestInit): Promise<McpResponse> {
  return request<McpResponse>("POST", "/mcp", req, init);
}

/**
 * Event WebSocket ticket.
 * POST /events/ticket · operator only
 */
export type EventsTicketRequest = Record<string, never>;
/** Untyped JSON: no schema on the node describes it yet. */
export type EventsTicketResponse = unknown;
export function eventsTicket(req: EventsTicketRequest = {}, init?: RequestInit): Promise<EventsTicketResponse> {
  return request<EventsTicketResponse>("POST", "/events/ticket", req, init);
}

/** Every node route: its client function, its risk grade for the Baker
 * (`null`: operator only, never offered to an agent) and what it does. */
export const ROUTES: ReadonlyArray<{
  name: string;
  method: Method;
  path: string;
  risk: Risk | null;
  hidden: boolean;
  summary: string;
}> = [
  { name: "nodeHealth", method: "GET", path: "/health", risk: "read", hidden: false, summary: "Whether the node is up." },
  { name: "node", method: "GET", path: "/node", risk: null, hidden: true, summary: "This node's identity and policy." },
  { name: "nodeVersions", method: "GET", path: "/versions", risk: "read", hidden: false, summary: "What this node runs: Bakery build, Node.js, the Claude adapter and engine, each live session's runtime." },
  { name: "agents", method: "GET", path: "/agents", risk: "read", hidden: false, summary: "Every agent and its runtime, model, state and conversation." },
  { name: "agentsToolPolicy", method: "GET", path: "/agents/tool-policy", risk: "read", hidden: false, summary: "The operator's node-wide policy per native tool kind: always, ask or never." },
  { name: "agentsToolPolicySet", method: "POST", path: "/agents/tool-policy", risk: null, hidden: true, summary: "Set the node-wide native tool policy." },
  { name: "agentsAlwaysAllowed", method: "GET", path: "/agents/always-allowed", risk: "read", hidden: false, summary: "Every \"Always allow\" an approval card made: a kind of action, allowed without asking in one project, in the Baker's conversations, or in one chat." },
  { name: "agentsAlwaysAllowedRemove", method: "POST", path: "/agents/always-allowed/remove", risk: null, hidden: true, summary: "Take back one \"Always allow\": that kind asks again there." },
  { name: "agentsPeerLimit", method: "GET", path: "/agents/peer-limit", risk: "read", hidden: false, summary: "The most open peers agents may open in one project, or one workspace." },
  { name: "agentsPeerLimitSet", method: "POST", path: "/agents/peer-limit", risk: null, hidden: true, summary: "Set the most open peers agents may open in one project, or one workspace." },
  { name: "chatCreate", method: "POST", path: "/chat/create", risk: "run", hidden: false, summary: "Start a new chat in its own folder; with model_id and instruction it starts working right away." },
  { name: "chatMakeProject", method: "POST", path: "/chat/make-project", risk: null, hidden: true, summary: "Make a chat a project: its folder moves into Bakery's projects folder, or a folder chosen, as a Git repository, and the conversation carries on there." },
  { name: "conversationArchive", method: "POST", path: "/conversation/archive", risk: "write", hidden: false, summary: "Archive (archived:true) or restore any conversation: a chat, a project socket or a Baker thread." },
  { name: "conversationRename", method: "POST", path: "/conversation/rename", risk: "write", hidden: false, summary: "Rename any conversation: a chat, a project's thread or a Baker thread; the main Baker keeps its name." },
  { name: "runtimeRestart", method: "POST", path: "/conversation/restart", risk: "write", hidden: false, summary: "Stop a conversation's idle runtime; its next turn starts a fresh one with the whole history." },
  { name: "agentsStorage", method: "GET", path: "/agents/storage", risk: null, hidden: true, summary: "Runtime home storage." },
  { name: "agentsStorageList", method: "GET", path: "/agents/storage/list", risk: null, hidden: true, summary: "List a runtime home folder." },
  { name: "agentsStorageFile", method: "GET", path: "/agents/storage/file", risk: null, hidden: true, summary: "Read a runtime home file." },
  { name: "agentsStorageDelete", method: "POST", path: "/agents/storage/delete", risk: null, hidden: true, summary: "Delete runtime home storage." },
  { name: "machineAccess", method: "GET", path: "/machine-access", risk: "read", hidden: false, summary: "Which folders, programs and variables agents may reach, and what stays denied under any grant." },
  { name: "machineAccessSet", method: "POST", path: "/machine-access", risk: null, hidden: true, summary: "Set machine access." },
  { name: "machineAccessCheck", method: "POST", path: "/machine-access/check", risk: null, hidden: true, summary: "Probe the sandbox." },
  { name: "machineAccessRequestsDecide", method: "POST", path: "/machine-access/requests/decide", risk: null, hidden: true, summary: "Decide a folder access request." },
  { name: "machineAccessReach", method: "POST", path: "/machine-access/reach", risk: null, hidden: true, summary: "Set one agent's reach on the scope dial." },
  { name: "machineAccessContainers", method: "POST", path: "/machine-access/containers", risk: null, hidden: true, summary: "Turn one agent's containers switch on or off." },
  { name: "machineAccessSeatSize", method: "POST", path: "/machine-access/seat-size", risk: null, hidden: true, summary: "Set one container seat's CPUs and memory, or return it to the default sized from this Mac." },
  { name: "machineAccessClaudeContainerSignIn", method: "POST", path: "/machine-access/claude-container/sign-in", risk: null, hidden: true, summary: "Sign Claude in for container seats, in a terminal pane." },
  { name: "containersSeatImage", method: "GET", path: "/containers/seat-image", risk: "read", hidden: false, summary: "Whether the seat image is built from this node's recipe: missing, stale or current." },
  { name: "seatImageBuild", method: "POST", path: "/containers/seat-image/build", risk: "run", hidden: false, summary: "Build the seat image from this node's recipe, in a terminal pane the operator sees." },
  { name: "containersList", method: "GET", path: "/containers", risk: "read", hidden: false, summary: "Containers, images, volumes, networks or compose projects on Apple container or Docker, as rows both backends share; a container Bakery started names its role, conversation and seat." },
  { name: "containersInspect", method: "GET", path: "/containers/inspect", risk: "read", hidden: false, summary: "One container, image, volume, network or compose project, as the backend describes it." },
  { name: "containersLogs", method: "GET", path: "/containers/logs", risk: "read", hidden: false, summary: "A container's last log lines." },
  { name: "containersStats", method: "GET", path: "/containers/stats", risk: "read", hidden: false, summary: "CPU, memory and I/O of every running container on a backend." },
  { name: "containersStart", method: "POST", path: "/containers/start", risk: "run", hidden: false, summary: "Start a stopped container." },
  { name: "containersStop", method: "POST", path: "/containers/stop", risk: "run", hidden: false, summary: "Stop a running container." },
  { name: "containersRestart", method: "POST", path: "/containers/restart", risk: "run", hidden: false, summary: "Restart a container, or a whole compose project (kind compose): it stops and starts again, its files kept. The Bakery's own are refused while in use and otherwise start fresh: an agent's sandbox with its next turn, a conversation's editor the next time Code opens." },
  { name: "containersRemove", method: "POST", path: "/containers/remove", risk: "destructive", hidden: false, summary: "Remove a stopped container, an image, a volume or a network, or take a compose project down." },
  { name: "containersPrune", method: "POST", path: "/containers/prune", risk: "destructive", hidden: false, summary: "Remove every unused container, image, volume or network on a backend." },
  { name: "containersComposeUp", method: "POST", path: "/containers/compose/up", risk: "destructive", hidden: false, summary: "Start a Docker compose project from its file, detached." },
  { name: "containersRun", method: "POST", path: "/containers/run", risk: "run", hidden: false, summary: "Start a container from an image, detached, its ports on 127.0.0.1 only; for an agent, what it can't reach stays out of its mounts." },
  { name: "codeOpen", method: "POST", path: "/code/open", risk: null, hidden: true, summary: "Open a conversation's worktree in the operator's editor (openvscode-server in a container of its own) at a 127.0.0.1 URL that carries its connection token." },
  { name: "codeClose", method: "POST", path: "/code/close", risk: null, hidden: true, summary: "Stop a conversation's editor." },
  { name: "codeAppearance", method: "POST", path: "/code/appearance", risk: null, hidden: true, summary: "The scheme the app shows now (its colors as the app resolves them, light or dark, its code font), for every Code editor: open ones follow at once. The app sends it when its scheme or the Mac's light or dark mode changes." },
  { name: "codeGoto", method: "POST", path: "/code/goto", risk: null, hidden: true, summary: "Open an edit row's file in its conversation's editor at the first line the edit changed, and say whether the file has changed since." },
  { name: "previewOpen", method: "POST", path: "/preview/open", risk: "run", hidden: false, summary: "A 127.0.0.1 URL for the dev server a conversation runs on a port, in its container or on this Mac." },
  { name: "modelProxy", method: "GET", path: "/model-proxy", risk: "read", hidden: false, summary: "The model proxy (CLIProxyAPI): whether it's installed and running, and the hosts its upstream calls may reach." },
  { name: "modelProxyInstall", method: "POST", path: "/model-proxy/install", risk: null, hidden: true, summary: "Install the pinned model proxy release, its sha256 checked, and start it." },
  { name: "modelProxyStart", method: "POST", path: "/model-proxy/start", risk: null, hidden: true, summary: "Start the installed model proxy again, after fixing what stopped it." },
  { name: "modelProxyKeys", method: "GET", path: "/model-proxy/keys", risk: "read", hidden: false, summary: "Which providers the model proxy holds API keys for, and how many; never a key." },
  { name: "modelProxyKeysSet", method: "POST", path: "/model-proxy/keys", risk: null, hidden: true, summary: "Give the model proxy an API key for a provider." },
  { name: "modelProxyKeysRemove", method: "POST", path: "/model-proxy/keys/remove", risk: null, hidden: true, summary: "Take an API key back from the model proxy, by its fingerprint." },
  { name: "metrics", method: "GET", path: "/metrics", risk: null, hidden: true, summary: "The model proxy's numbers in Prometheus's text format: calls, tokens and latency by agent, provider and model, and calls another model answered." },
  { name: "agentsMap", method: "GET", path: "/agents/map", risk: "read", hidden: false, summary: "Which conversation started which: the agent map." },
  { name: "versionsCheck", method: "POST", path: "/versions/check", risk: "read", hidden: false, summary: "Ask npm for the newest releases of the node's runtimes." },
  { name: "controlBakery", method: "GET", path: "/control/bakery", risk: "read", hidden: false, summary: "The deployed Bakery build and the last deploy, restart or rollback." },
  { name: "controlBakerySet", method: "POST", path: "/control/bakery", risk: null, hidden: true, summary: "Deploy, deploy only the UI (the open app reloads, nothing restarts), restart or roll back Bakery." },
  { name: "macAmphetamine", method: "GET", path: "/mac/amphetamine", risk: null, hidden: true, summary: "Amphetamine on this Mac: its session, the time left, closed-display mode." },
  { name: "macAmphetamineSet", method: "POST", path: "/mac/amphetamine", risk: null, hidden: true, summary: "Start or end an Amphetamine session, or turn its closed-display mode on or off, through Amphetamine's own scripting." },
  { name: "macFansOpen", method: "POST", path: "/mac/fans/open", risk: null, hidden: true, summary: "Open Macs Fan Control, which has no other way in." },
  { name: "macMeters", method: "GET", path: "/mac/meters", risk: null, hidden: true, summary: "This Mac's disk, memory, CPU and heat, each with its level (ok, warn, bad), and the last ten minutes of CPU and memory." },
  { name: "macHeat", method: "GET", path: "/mac/heat", risk: null, hidden: true, summary: "This Mac's thermal state, hottest sensor and fans, and whether agents' new turns wait for it to cool." },
  { name: "disk", method: "GET", path: "/disk", risk: null, hidden: true, summary: "Where this Mac's disk goes for the Bakery: free space, each thread's build folders, every image, container and volume on Docker and Apple container with the project that uses it, Colima's disk and the Bakery's own data." },
  { name: "diskClean", method: "POST", path: "/disk/clean", risk: null, hidden: true, summary: "One cleanup the operator picked and confirmed: a listed build folder, the images no container uses (never the Bakery's own), stopped containers, Docker's build cache, or the volumes they ticked by name." },
  { name: "engine", method: "GET", path: "/engine", risk: null, hidden: true, summary: "What this Mac runs for the Bakery: Apple container, Tailscale and its machines, Colima, Homebrew's services, cloudflared, Supabase's local stacks; for Colima's default VM, what it uses inside, per container and of this Mac, its peak, a suggested size, the size waiting for its next start and its idle stop." },
  { name: "engineColimaIdleStop", method: "POST", path: "/engine/colima/idle-stop", risk: null, hidden: true, summary: "Turn Colima's idle stop on or off: its default VM stops once nothing has used Docker for a while, and a Docker action starts it again." },
  { name: "engineDockerDefault", method: "POST", path: "/engine/docker-default", risk: null, hidden: true, summary: "Pick the Bakery's Docker engine for projects that pick none: colima or orbstack (installed first); it answers what the switch means for images and volumes already there." },
  { name: "engineOrbstackInstall", method: "POST", path: "/engine/orbstack/install", risk: null, hidden: true, summary: "The operator's yes to installing OrbStack with Homebrew; it installs in the background and the app hears how it went." },
  { name: "engineColimaSize", method: "POST", path: "/engine/colima/size", risk: null, hidden: true, summary: "Pick the memory (and CPUs) Colima's default VM starts with next, or take a size back before it applies." },
  { name: "serviceCopy", method: "POST", path: "/project/service/copy", risk: "write", hidden: false, summary: "Start a separate copy of one of this project's services (redis, postgres) for destructive work, with its own data and port, reachable by this thread alone. Stop it with service_copy_stop when done." },
  { name: "serviceCopyStop", method: "POST", path: "/project/service/copy/stop", risk: "write", hidden: false, summary: "Stop this thread's copy of a service, by its port; its data goes with it." },
  { name: "filesReadAsks", method: "GET", path: "/files/read/asks", risk: null, hidden: true, summary: "A project's secret-named files: the operator's answers on which its builds may read, and the files its rooms were refused that wait on them." },
  { name: "filesReadDecide", method: "POST", path: "/files/read/decide", risk: null, hidden: true, summary: "The operator's answer for a secret-named file a project's build needs: allow it (readable, never writable, in its rooms), deny it, or forget the answer. A private key takes sure: true." },
  { name: "controlRestartNow", method: "POST", path: "/control/restart/now", risk: null, hidden: true, summary: "A restart waiting for running turns goes ahead at once; the turns it cuts off carry on after it." },
  { name: "clientLog", method: "POST", path: "/client/log", risk: null, hidden: true, summary: "Client log line." },
  { name: "policyWriteMode", method: "POST", path: "/policy/write-mode", risk: null, hidden: true, summary: "Set the node-wide write mode." },
  { name: "projectInspect", method: "POST", path: "/project/inspect", risk: "read", hidden: false, summary: "What a folder is: git repo, branch, worktrees, stacks. Changes nothing." },
  { name: "projectOpen", method: "POST", path: "/project/open", risk: "write", hidden: false, summary: "Open a folder as a project socket (attach a branch or create a worktree)." },
  { name: "projectCreate", method: "POST", path: "/project/create", risk: "write", hidden: false, summary: "Create a Project: its own folder inside Bakery (root {mode:\"bakery\"}) or an existing folder ({mode:\"attach\",path})." },
  { name: "projectRename", method: "POST", path: "/project/rename", risk: "write", hidden: false, summary: "Rename a Project; its project_id never changes." },
  { name: "projectControl", method: "GET", path: "/project/control", risk: "read", hidden: false, summary: "A project's control plane, read from its files now: per folder what it's built with, its direct dependencies and tools; the MCP servers it defines plus the Bakery's own; its skills, once each; its instruction files; and the Bakery's own setup for it with what its files suggest. Every item names its source file." },
  { name: "projectSetup", method: "POST", path: "/project/setup", risk: null, hidden: true, summary: "Set the Bakery's own setup for a project (toolchain manager, task runner, containers, secrets); each agent's next turn uses it." },
  { name: "networkHosts", method: "GET", path: "/network/hosts", risk: "read", hidden: false, summary: "A project's network list: the hosts its threads' sandboxes reach (the defaults, the hosts its files point to, and the operator's answers), each with where it comes from, and the hosts waiting on the operator." },
  { name: "networkHostDecide", method: "POST", path: "/network/host/decide", risk: null, hidden: true, summary: "The operator's answer to a host for a project: allow, deny, or null to forget it so it asks again. It resolves each open ask it covers, in the thread that asked." },
  { name: "projectToolchainKeep", method: "POST", path: "/project/toolchain/keep", risk: null, hidden: true, summary: "Keep one of a Mac sandbox project's toolchain additions for good: the operator's own FVM, mise or gems gets it, for every project." },
  { name: "toolchainRequest", method: "POST", path: "/project/toolchain/request", risk: "read", hidden: false, summary: "Ask the operator to install a toolchain or version into this Mac sandbox project's own copies: a Flutter version (tool flutter), a mise tool's version (tool mise, name node), or a gem. Their answer, and how the install went, come back to this conversation." },
  { name: "projectToolchainDecide", method: "POST", path: "/project/toolchain/decide", risk: null, hidden: true, summary: "The operator's answer to an install ask: yes installs it into the project's own copies and tells the thread how it went; no tells the thread so." },
  { name: "threadDoor", method: "GET", path: "/thread/door", risk: null, hidden: true, summary: "A thread's door on this Mac, as a proxy URL for the app's Preview of it: the Preview then reaches only what the thread's project list allows." },
  { name: "threadToolchains", method: "GET", path: "/thread/toolchains", risk: null, hidden: true, summary: "The toolchains a thread's folder pins (mise's tools, Rust, FVM's Flutter, Xcode) beside what its own room runs, asked inside that room." },
  { name: "threadIphone", method: "GET", path: "/thread/iphone", risk: null, hidden: true, summary: "Whether a thread has an unsigned iPhone build for Build to my iPhone, looked for where the button looks." },
  { name: "threadIphoneRun", method: "POST", path: "/thread/iphone/run", risk: null, hidden: true, summary: "Run on my iPhone (as me): types flutter run for the thread's app, on the paired iPhone, into a new terminal of the operator's own in its worktree. Nothing runs until the operator presses Enter; then it runs as them." },
  { name: "threadIphoneInstall", method: "POST", path: "/thread/iphone/install", risk: null, hidden: true, summary: "Build to my iPhone: the thread's newest unsigned iOS build (flutter build ios --no-codesign), signed with the operator's own certificate for their provisioning profile and installed on their paired iPhone." },
  { name: "projectPin", method: "POST", path: "/project/pin", risk: "write", hidden: false, summary: "Pin or unpin a Project in the sidebar." },
  { name: "projectRetire", method: "POST", path: "/project/retire", risk: "write", hidden: false, summary: "Remove a Project from Bakery's lists; never deletes its files, roots, sockets, memory or history." },
  { name: "socketOpen", method: "POST", path: "/project/root/open", risk: "write", hidden: false, summary: "Open a new directory-root socket on an existing Project." },
  { name: "projectSocketAttach", method: "POST", path: "/project/socket/attach", risk: "write", hidden: false, summary: "Attach an existing socket to a Project." },
  { name: "projectSocketDetach", method: "POST", path: "/project/socket/detach", risk: "write", hidden: false, summary: "Detach a socket from its Project." },
  { name: "projectRollup", method: "GET", path: "/project/rollup", risk: "read", hidden: false, summary: "A Project at a glance: each repo's open pull requests (checks pass/fail/pending, review) and latest CI run per branch and workflow (gh), and each worktree's branch, lines added and removed since it forked, and whether it has uncommitted changes." },
  { name: "socketList", method: "GET", path: "/streams", risk: "read", hidden: false, summary: "Every conversation (socket) this node knows, most recently active first." },
  { name: "gatesList", method: "GET", path: "/gates", risk: "read", hidden: false, summary: "Every pending write proposal across the node, oldest first." },
  { name: "heart", method: "GET", path: "/heart", risk: "read", hidden: false, summary: "Event-log statistics for the node or one stream." },
  { name: "memoryRead", method: "GET", path: "/memory", risk: "read", hidden: false, summary: "A stream's durable memory entries." },
  { name: "memoryWrite", method: "POST", path: "/memory/write", risk: "write", hidden: false, summary: "Write a durable memory note on a stream." },
  { name: "toolkit", method: "GET", path: "/toolkit", risk: "read", hidden: false, summary: "The Bakery's toolkit: its skills, MCP servers, hooks, plugins and instructions, Bakery-wide and per project, each with how every runtime gets it." },
  { name: "toolkitSet", method: "POST", path: "/toolkit", risk: null, hidden: true, summary: "Add a toolkit item, or change one (with its id)." },
  { name: "toolkitRemove", method: "POST", path: "/toolkit/remove", risk: null, hidden: true, summary: "Remove a toolkit item." },
  { name: "toolkitEnable", method: "POST", path: "/toolkit/enable", risk: null, hidden: true, summary: "Turn a toolkit item on or off." },
  { name: "toolkitPort", method: "GET", path: "/toolkit/port", risk: null, hidden: true, summary: "The VS Code tool port: what the operator's Claude Code setup (~/.claude and ~/.claude.json) holds, read now: its instructions and rules, skills, MCP servers, hooks and plugins, and what has no Toolkit kind yet; each with what it becomes, the keys left out (by name), what changes on the way, and the Toolkit item it would change. The operator's only." },
  { name: "toolkitPortSet", method: "POST", path: "/toolkit/port", risk: null, hidden: true, summary: "Bring the ticked items of the operator's Claude Code setup into the Toolkit, Bakery-wide or for one project; one already there is changed. No key or secret comes across. The operator's only." },
  { name: "learning", method: "GET", path: "/learning", risk: null, hidden: true, summary: "The memory's learning loop: its settings (each part on or off, every trigger, time and limit, the models, the projects in it, the pause), what holds parts back now, the models it may ask, when the nightly run next starts, and each part's last run." },
  { name: "learningSet", method: "POST", path: "/learning", risk: null, hidden: true, summary: "Change any of the learning loop's settings: what is sent is laid over them." },
  { name: "learningRun", method: "POST", path: "/learning/run", risk: null, hidden: true, summary: "Run one part of the learning loop now (a thread-end review names its thread), whatever the pause and the time; it runs on behind the answer." },
  { name: "learningRuns", method: "GET", path: "/learning/runs", risk: null, hidden: true, summary: "The learning loop's runs, the newest first: what each did, the notes it touched, the proposals it made and the model calls it spent." },
  { name: "memorySearch", method: "POST", path: "/memory/search", risk: "read", hidden: false, summary: "Search the Bakery's memory: the notes you see (your thread's, your project's and the Bakery's) that match, best first, a line each with how much evidence it rests on and whether it is stale. Read the ones you need whole with memory_get." },
  { name: "memoryGet", method: "POST", path: "/memory/get", risk: "read", hidden: false, summary: "Read notes whole, by id, within a token budget: title, summary, body, why, how to apply, links, evidence, versions and how each is used." },
  { name: "memoryRelated", method: "POST", path: "/memory/related", risk: "read", hidden: false, summary: "The notes linked to one, either way, one or two links away." },
  { name: "memoryNotes", method: "GET", path: "/memory/notes", risk: "read", hidden: false, summary: "Every kept note, newest first, with how each is used and whether it is stale: the wiki's pages." },
  { name: "memoryNote", method: "GET", path: "/memory/note", risk: "read", hidden: false, summary: "One note's page: the note, how it is used, and every version." },
  { name: "memoryPropose", method: "POST", path: "/memory/propose", risk: "write", hidden: false, summary: "Propose a note for the Bakery's memory: only what will still matter in six months and is hard to find again from the code, git or the thread's history, resting on evidence. Never propose a secret; task progress, a todo or a log of finished work; what the code or its docs already say; the story of a one-off task; a setup failure the operator can fix (a missing tool, an unset credential); a claim that a tool or feature doesn't work; an error that went away on a retry (the retry is the lesson); or attempts that never worked (only a fix you know works). It waits for the operator, who keeps it, edits it or declines it; a repeat of a kept, waiting or declined note adds nothing." },
  { name: "memoryRate", method: "POST", path: "/memory/rate", risk: "write", hidden: false, summary: "Say whether a note you used helped or hurt the work; your latest word on a note counts." },
  { name: "memoryProposals", method: "GET", path: "/memory/proposals", risk: "read", hidden: false, summary: "Notes agents proposed: those waiting for the operator (or kept, or declined), the oldest first." },
  { name: "memoryReview", method: "POST", path: "/memory/review", risk: null, hidden: true, summary: "Keep a proposed note, as it is or edited (a wider scope too), or decline it; a declined one never comes back." },
  { name: "memoryKeepAll", method: "POST", path: "/memory/keep-all", risk: null, hidden: true, summary: "Keep every waiting proposal at once, as it is (or those named)." },
  { name: "memoryEdit", method: "POST", path: "/memory/edit", risk: null, hidden: true, summary: "Edit a note (a new version), or write a new one with its type, scope, title and body." },
  { name: "memoryPromote", method: "POST", path: "/memory/promote", risk: null, hidden: true, summary: "Promote an agent's scratch note into a shared, kept note in a scope (bakery, baker, project:<id> or workspace:<thread>), edited if wanted; its versions stay." },
  { name: "memoryRetire", method: "POST", path: "/memory/retire", risk: null, hidden: true, summary: "Retire a note: it leaves the wiki and every search; its history stays." },
  { name: "memoryDigest", method: "GET", path: "/memory/digest", risk: "read", hidden: false, summary: "The memory's digest: proposals waiting for the operator (Keep all keeps them), notes gone stale, and the last day." },
  { name: "memoryTimeline", method: "GET", path: "/memory/timeline", risk: "read", hidden: false, summary: "What happened in the memory, the newest first: proposed, kept, edited, written, declined and retired." },
  { name: "backlogList", method: "GET", path: "/backlog", risk: "read", hidden: false, summary: "The backlog: what is to be done next, for the Bakery or a project, in the operator's order: each item's why, status (queued, doing, done, dropped), who added it, its threads and its history. An agent sees its project's items and the Bakery's." },
  { name: "backlogAdd", method: "POST", path: "/backlog/add", risk: "write", hidden: false, summary: "Add an item to the end of the backlog: something worth doing later, with why. An agent's goes into its project's backlog unless it names the scope (bakery, or a project), marked as its own." },
  { name: "backlogEdit", method: "POST", path: "/backlog/edit", risk: "write", hidden: false, summary: "Change a backlog item's title, why, scope or linked threads." },
  { name: "backlogStatus", method: "POST", path: "/backlog/status", risk: "write", hidden: false, summary: "Move a backlog item along: queued, doing, done or dropped." },
  { name: "backlogMove", method: "POST", path: "/backlog/move", risk: null, hidden: true, summary: "Put a backlog item just above another, or at the end." },
  { name: "backlogRemove", method: "POST", path: "/backlog/remove", risk: null, hidden: true, summary: "Remove a backlog item." },
  { name: "skillRead", method: "GET", path: "/toolkit/skill", risk: "read", hidden: false, summary: "Read a skill your conversation has, by name: when one fits the work, read it before you start." },
  { name: "conversationBrief", method: "GET", path: "/conversation/brief", risk: "read", hidden: false, summary: "A conversation's brief: the standing instructions its agent gets every turn, who set it, whether an agent's still waits for the operator, and every change." },
  { name: "briefSet", method: "POST", path: "/conversation/brief", risk: "write", hidden: false, summary: "Set another conversation's brief. The operator's holds as theirs; the Baker's, or an orchestrator's in the same project, is a recommendation, in effect until the operator turns it down. An agent never sets its own." },
  { name: "conversationBriefDecide", method: "POST", path: "/conversation/brief/decide", risk: null, hidden: true, summary: "Keep, or turn down, an agent's brief; turned down, the brief before it holds again." },
  { name: "moveInSessions", method: "GET", path: "/move-in/sessions", risk: null, hidden: true, summary: "The operator's Claude Code sessions of the Bakery's own folders, newest first, to move into the Bakery: each one's transcript, its size, how often it was compacted, its title (its custom title first), the conversation it moved into, and where it can go on: a new workspace, a workspace's orc, a worker under one in its worktree, the Baker's own conversation or a Baker thread, the best fit suggested." },
  { name: "moveInImport", method: "POST", path: "/move-in/import", risk: null, hidden: true, summary: "Move a Claude Code session into the Bakery with nothing lost, where the operator picks: one of its destinations (a new workspace's orc, a worker under a workspace in the worktree it has, the Baker's own conversation, a Baker thread, or a conversation already there). A copy of its transcript goes where that conversation's Claude runtime reads sessions, and its next turn resumes it. Again, what the original gained since comes along while the thread has not carried it on. Refused (409), with nothing changed: a conversation carrying on another session or in the middle of a turn, a session already moved in elsewhere, and one both sides went on with. The original is only read." },
  { name: "moveInHistory", method: "GET", path: "/move-in/history", risk: null, hidden: true, summary: "A moved-in conversation's old conversation, read from the transcript copy its move-in made, as it came in: the operator's and Claude's messages, tool calls in short form (what a tool answered is never shown, only counted), compactions marked with their summary. Newest first, a page at a time: pass the last page's `earlier` as `before`. Keys and tokens are left out; what a page reads past without showing is counted in `skipped`, a line that can't be read says so, and a text cut short says how long it was." },
  { name: "freshStartPlan", method: "GET", path: "/fresh-start/plan", risk: null, hidden: true, summary: "What a fresh start would do now: the Baker thread it carries over (the one named, else the one called local models), the conversations it leaves behind in the old Heart, the Always-allow grants it clears, the worktrees left on disk, and why it can't go ahead (a turn running, or no single thread to keep)." },
  { name: "threadMerge", method: "POST", path: "/thread/merge", risk: null, hidden: true, summary: "Merge work waiting in the review where it goes: a chef's branch into main, as its spawner's thread_merge does (then the chef's thread is archived and its worktree and branch are removed, anything not committed going to the Trash first); a workspace's branch into its repository's main, or a workspace worker's into its workspace, only on the operator's approval and exactly as the review showed it. As a merge commit, into a clean checkout; a conflict merges nothing." },
  { name: "mergeWaiting", method: "GET", path: "/merge/waiting", risk: "read", hidden: false, summary: "Work waiting to merge, newest first: chefs' and workspaces' branches for main, and workspace workers' branches their orc proposed for its workspace. Each with its branch, what it merges into, its commits, the files it changes, and lines added and removed." },
  { name: "mergeReady", method: "POST", path: "/merge/ready", risk: "write", hidden: false, summary: "Propose work for the merge review: a workspace's orc proposes a worker's branch for its workspace, or its own branch for main. Everything committed; a note says what it is and how it was tested. Asking again replaces it." },
  { name: "workspace", method: "POST", path: "/workspace", risk: "write", hidden: false, summary: "Open a workspace: an orchestrator's conversation on a worktree of its own, a new branch off its repository's main, which opens workers as its peers. The Bakery's own beneath the Baker, or a catalog project's (project_id) on that project's repository; its work merges into that project's main through the merge review." },
  { name: "deletePlan", method: "POST", path: "/delete/plan", risk: null, hidden: true, summary: "What deleting a project, one repository of a project, a workspace, or a thread or chat would remove and what stays, changing nothing: its threads, the worktrees and branches the Bakery made (unsaved work in them kept in git first), rooms, containers, volumes, networks, the engines it could have used that are off (started to clear, then stopped), folders the Bakery made going to the Trash, the operator's own folders the trash_own tick takes, project roots let go, folders and branches kept because something staying uses them, why it can't go ahead (the main Baker, a turn running), and its digest." },
  { name: "deleteConfirm", method: "POST", path: "/delete/confirm", risk: null, hidden: true, summary: "Carry out exactly the plan the operator saw, while a fresh plan still has its digest (409 when it changed, or is refused): unsaved work kept in git first (nothing goes that couldn't be kept), each thread discarded with its history kept, what the plan removes removed, engines it could have used that were off started and stopped again, folders to the Trash, a project retired. Recorded as delete.done; answers what went and each step that didn't." },
  { name: "trash", method: "GET", path: "/trash", risk: null, hidden: true, summary: "The Bakery's Trash: unsaved work deletes kept in git, in every repository the Bakery worked in, newest first: repo, name, when, its commits outside main and its uncommitted changes. Each stays until the operator empties the Trash." },
  { name: "trashPutBack", method: "POST", path: "/trash/put-back", risk: null, hidden: true, summary: "Put an entry of the Trash back: its branch made again at its tip and a worktree on it, its uncommitted changes restored exactly; it leaves the Trash." },
  { name: "trashEmpty", method: "POST", path: "/trash/empty", risk: null, hidden: true, summary: "Empty the Trash of the entries the operator saw and confirmed, one or all of them: the unsaved work in them is gone for good. Entries kept since are not touched; answers how many went and each that couldn't." },
  { name: "sendMessage", method: "POST", path: "/message/send", risk: "write", hidden: false, summary: "Send a message or a note to another conversation, as SendMessage does between sessions: one in your project, any Baker conversation when you are one, or the Baker (to: baker). A message starts a turn there, after any turn running, so send one only when they must act on it. Anything else goes as a note (note: true): it shows there without starting a turn, and their next turn reads it. Your conversation shows it sent, then taken up or read." },
  { name: "saved", method: "GET", path: "/saved", risk: null, hidden: true, summary: "The operator's saved replies, newest first: one conversation's, or every conversation's." },
  { name: "savedSet", method: "POST", path: "/saved", risk: null, hidden: true, summary: "Save a finished reply by its agent.turn.done event; saving it again answers the one already kept." },
  { name: "savedRemove", method: "POST", path: "/saved/remove", risk: null, hidden: true, summary: "Unsave a reply." },
  { name: "savedTick", method: "POST", path: "/saved/tick", risk: null, hidden: true, summary: "Tick or untick one checklist item of a saved reply." },
  { name: "memoryImport", method: "POST", path: "/memory/import", risk: null, hidden: true, summary: "Import a folder of Claude Code memory notes into a conversation's memory (a Baker thread's into the one Baker memory), keeping each note's name, type, description and links." },
  { name: "context", method: "GET", path: "/context", risk: "read", hidden: false, summary: "The context a stream's next turn starts from." },
  { name: "worktree", method: "POST", path: "/worktree", risk: "write", hidden: false, summary: "Create a named git worktree for a project (legacy shape of project/open)." },
  { name: "envProvision", method: "POST", path: "/env/provision", risk: "run", hidden: false, summary: "Build a stream's sandbox room (its toolchain image)." },
  { name: "envExec", method: "POST", path: "/env/exec", risk: "run", hidden: false, summary: "Run one command (argv, no shell) in a stream's sandbox room and return its output." },
  { name: "envPty", method: "POST", path: "/env/pty", risk: null, hidden: true, summary: "Interactive terminal attach." },
  { name: "env", method: "GET", path: "/env", risk: "read", hidden: false, summary: "A stream's sandbox room status." },
  { name: "terminalCreate", method: "POST", path: "/terminal/create", risk: null, hidden: true, summary: "Open an interactive terminal: a thread's (its stream_id, host or room), this Mac's from anywhere (stream_id sys:host: a login shell in the operator's home), or a project's (stream_id project:<id>: in its main folder, on this Mac, in its room or in a container as its sandbox setting says; mode is then ignored)." },
  { name: "terminalList", method: "GET", path: "/terminal/list", risk: "read", hidden: false, summary: "The studio terminals open on a stream: a thread's, or project:<id> for a project's." },
  { name: "terminalRename", method: "POST", path: "/terminal/rename", risk: "write", hidden: false, summary: "Rename a studio terminal." },
  { name: "terminalRead", method: "GET", path: "/terminal/read", risk: "read", hidden: false, summary: "What one of the thread's terminals printed last, as text, and whether something is running in it." },
  { name: "terminalClose", method: "POST", path: "/terminal/close", risk: "destructive", hidden: false, summary: "Close a studio terminal and kill its process tree." },
  { name: "terminalTicket", method: "POST", path: "/terminal/ticket", risk: null, hidden: true, summary: "Terminal connection ticket." },
  { name: "terminalWs", method: "GET", path: "/terminal/ws", risk: null, hidden: true, summary: "Terminal WebSocket." },
  { name: "jobList", method: "GET", path: "/jobs", risk: "read", hidden: false, summary: "Jobs started with job_start, running or ended, newest first." },
  { name: "jobStart", method: "POST", path: "/jobs/start", risk: "run", hidden: false, summary: "Start a long command (a build, tests, a dev server) in a conversation's sandbox and return at once. It shows live in a studio pane, and the conversation that started it (yours, when you call this) is told when it ends." },
  { name: "jobOutput", method: "GET", path: "/jobs/output", risk: "read", hidden: false, summary: "A job's output from an offset (since 0, then the last call's next), with its state and exit code." },
  { name: "jobStop", method: "POST", path: "/jobs/stop", risk: "write", hidden: false, summary: "Stop a running job." },
  { name: "gitStatus", method: "GET", path: "/git/status", risk: "read", hidden: false, summary: "A conversation's checkout: branch, upstream, ahead/behind, and each changed file, staged or not." },
  { name: "gitBranches", method: "GET", path: "/git/branches", risk: "read", hidden: false, summary: "Local branches with the current one marked, their upstream and tracking, and last commit." },
  { name: "gitLog", method: "GET", path: "/git/log", risk: "read", hidden: false, summary: "Recent commits of the current branch or a named one." },
  { name: "gitDiff", method: "GET", path: "/git/diff", risk: "read", hidden: false, summary: "The diff of unstaged changes, staged ones, one file, one commit, or a whole branch against a base, what is not committed yet too if asked." },
  { name: "gitStage", method: "POST", path: "/git/stage", risk: "write", hidden: false, summary: "Stage paths, or every change; or take them out of the staged set." },
  { name: "gitCommit", method: "POST", path: "/git/commit", risk: "write", hidden: false, summary: "Commit what is staged, with a message." },
  { name: "gitBranch", method: "POST", path: "/git/branch", risk: "write", hidden: false, summary: "Create a branch, from the current commit or a named one, and switch to it." },
  { name: "gitSwitch", method: "POST", path: "/git/switch", risk: "write", hidden: false, summary: "Switch the checkout to another branch." },
  { name: "gitFetch", method: "POST", path: "/git/fetch", risk: "read", hidden: false, summary: "Fetch a remote's branches (default origin) without touching the checkout; status then shows ahead/behind." },
  { name: "gitPull", method: "POST", path: "/git/pull", risk: "write", hidden: false, summary: "Bring the current branch up to its upstream with a merge, never a rebase; a conflict aborts it and changes nothing." },
  { name: "gitUpdate", method: "POST", path: "/git/update", risk: "write", hidden: false, summary: "Bring a thread up to the branch it forked from with a merge, never a rebase, fetching it first; a conflict stops it and lists the files to resolve." },
  { name: "gitRebase", method: "POST", path: "/git/rebase", risk: "write", hidden: false, summary: "Rebase a thread onto a branch, with the reason you rebase instead of merging (git_update) recorded first; an agent's own git rebase is refused without one. A conflict stops it and lists the files, the rebase left in progress." },
  { name: "gitPush", method: "POST", path: "/git/push", risk: "destructive", hidden: false, summary: "Push a branch to origin and track it. Never forced." },
  { name: "googleChange", method: "POST", path: "/google/change", risk: "destructive", hidden: false, summary: "The operator's say on one change to Google Cloud with their own sign-in (anything but a read). The thread's door asks this itself when its room sends one, holds the request, and sends it once approved; calling it yourself sends nothing." },
  { name: "gitPr", method: "POST", path: "/git/pr", risk: "destructive", hidden: false, summary: "Open a GitHub pull request for the current branch (gh)." },
  { name: "gitMerge", method: "POST", path: "/git/merge", risk: "destructive", hidden: false, summary: "Merge a branch into the current one; a conflict aborts it and changes nothing." },
  { name: "gitMergePreview", method: "GET", path: "/git/merge-preview", risk: "read", hidden: false, summary: "What merging the current branch into base would change on base, its conflicts included, without touching the checkout (git merge-tree): each file with its lines added and removed (the first 200, all counted); with diff, the diff too, cut at 32 KiB." },
  { name: "gitMergePreviewChain", method: "GET", path: "/git/merge-preview/chain", risk: "read", hidden: false, summary: "Merge several branches into base in turn (base with the first, that with the next, and so on) without touching the checkout: each step's conflicts, so branches that conflict only with each other show up, then each file with its lines (the first 200, all counted); with diff, the combined diff too, cut at 32 KiB." },
  { name: "gitPrChecks", method: "GET", path: "/git/pr/checks", risk: "read", hidden: false, summary: "The checks on the current branch's pull request and how each stands (gh)." },
  { name: "githubRepos", method: "GET", path: "/github/repos", risk: "read", hidden: false, summary: "The operator's GitHub repositories and their organizations': visibility, default branch, when updated, and where each is cloned on this Mac, if it is." },
  { name: "githubClone", method: "POST", path: "/github/clone", risk: "write", hidden: false, summary: "Clone a GitHub repository into a new folder in the operator's home (~/Dev/code/<name> by default) and open it as a Project." },
  { name: "githubIssues", method: "GET", path: "/github/issues", risk: "read", hidden: false, summary: "A repository's issues: open by default, most recent first." },
  { name: "githubIssue", method: "GET", path: "/github/issue", risk: "read", hidden: false, summary: "One issue with its body and comments." },
  { name: "githubIssueCreate", method: "POST", path: "/github/issue/create", risk: "destructive", hidden: false, summary: "Open an issue on a repository. It is published under the operator's name, so it always asks first." },
  { name: "githubIssueComment", method: "POST", path: "/github/issue/comment", risk: "destructive", hidden: false, summary: "Comment on an issue. It is published under the operator's name, so it always asks first." },
  { name: "githubScope", method: "GET", path: "/github/scope", risk: "read", hidden: false, summary: "The pull request inbox's scope: the operator, their organizations and any other owner added, each on or off, and repositories turned on or off by name." },
  { name: "githubScopeSet", method: "POST", path: "/github/scope", risk: null, hidden: true, summary: "The operator turns owners and repositories on or off for the inbox; a login not listed yet is added." },
  { name: "githubPrs", method: "GET", path: "/github/prs", risk: "read", hidden: false, summary: "The open pull requests across the inbox's scope, most recently updated first: checks, reviews, mergeability, and the thread and project each belongs to." },
  { name: "githubPrsRefresh", method: "POST", path: "/github/prs/refresh", risk: "read", hidden: false, summary: "Ask GitHub for the inbox's pull requests again now." },
  { name: "githubPr", method: "GET", path: "/github/pr", risk: "read", hidden: false, summary: "One pull request in full: its text, files, diff, checks, reviews, review threads, comments and commits." },
  { name: "githubPrReview", method: "POST", path: "/github/pr/review", risk: null, hidden: true, summary: "The operator reviews a pull request: a comment, an approval or a request for changes, with line comments." },
  { name: "githubPrComment", method: "POST", path: "/github/pr/comment", risk: null, hidden: true, summary: "The operator comments on a pull request, or replies in one of its review threads." },
  { name: "githubPrMerge", method: "POST", path: "/github/pr/merge", risk: null, hidden: true, summary: "The operator merges a pull request, or squashes it, deleting its branch if asked." },
  { name: "githubPrReady", method: "POST", path: "/github/pr/ready", risk: null, hidden: true, summary: "The operator marks a draft pull request ready for review." },
  { name: "githubPrThreadResolve", method: "POST", path: "/github/pr/thread/resolve", risk: null, hidden: true, summary: "The operator resolves a review thread, or opens it again." },
  { name: "githubRuns", method: "GET", path: "/github/runs", risk: "read", hidden: false, summary: "The latest CI run of each workflow on each branch, across the inbox's repositories and the operator's Projects'." },
  { name: "githubRun", method: "GET", path: "/github/run", risk: "read", hidden: false, summary: "One CI run with its jobs and their steps." },
  { name: "githubRunLog", method: "GET", path: "/github/run/log", risk: "read", hidden: false, summary: "A CI job's log, step by step; each step keeps its last 64 KiB." },
  { name: "githubRunRerun", method: "POST", path: "/github/run/rerun", risk: null, hidden: true, summary: "The operator re-runs a CI run, or only its failed jobs." },
  { name: "grants", method: "GET", path: "/grants", risk: "read", hidden: false, summary: "The grants a conversation holds: each lets it push one exact branch of one repository without asking first." },
  { name: "grantsGive", method: "POST", path: "/grants/give", risk: null, hidden: true, summary: "The operator lets a conversation push a branch without asking first: its current branch by default, fixed to that name now." },
  { name: "remoteWaiting", method: "GET", path: "/remote/waiting", risk: "read", hidden: false, summary: "Every remote action waiting for the operator across all conversations (pushes, pull requests, issues and comments), with their count." },
  { name: "remoteHistory", method: "GET", path: "/remote/history", risk: "read", hidden: false, summary: "The whole Bakery's remote actions done, newest first: pushes, pull requests, issues, comments, reviews, merges and re-runs, with who did them." },
  { name: "grantsRevoke", method: "POST", path: "/grants/revoke", risk: "write", hidden: false, summary: "End a grant: the operator takes it back, or the conversation's own agent gives it up." },
  { name: "gitProvenance", method: "GET", path: "/git/provenance", risk: "read", hidden: false, summary: "For each hunk a branch committed since base (base...HEAD), the recorded write that put it there: conversation, turn and tool call, with how many of its lines that write accounts for; none for a shell command, a hand edit or code from base." },
  { name: "uiLayout", method: "GET", path: "/ui/layout", risk: "read", hidden: false, summary: "The app's layout: workspaces, each a strip of columns of panels, plus the sidebar and the Baker dock. Null until one is saved." },
  { name: "uiLayoutSet", method: "POST", path: "/ui/layout", risk: "write", hidden: false, summary: "Save the app's layout; an open app follows it." },
  { name: "uiAppearance", method: "GET", path: "/ui/appearance", risk: "read", hidden: false, summary: "The app's appearance: its themes and the one in use. Null until one is saved." },
  { name: "uiAppearanceSet", method: "POST", path: "/ui/appearance", risk: "write", hidden: false, summary: "Save the app's appearance; an open app follows it." },
  { name: "uiBundle", method: "GET", path: "/ui/bundle", risk: "read", hidden: false, summary: "A conversation's own build of the app's UI (its bakery-client/build, from `pnpm --dir bakery-client build`): the hash the app checks it against, its size, the branch and commit it was built on, whether bakery-client has uncommitted changes, and the UI the app shows now." },
  { name: "uiBundleInstall", method: "POST", path: "/ui/bundle/install", risk: "destructive", hidden: false, summary: "Make a conversation's UI build, named by the hash ui_bundle answered, the app's UI: the open app checks it and reloads, nothing restarts. Always asks the operator, since the app runs it with their full access; refused if the build changed since. ui_bundle_rollback puts the previous UI back." },
  { name: "uiBundleRollback", method: "POST", path: "/ui/bundle/rollback", risk: "write", hidden: false, summary: "Put back the app's UI from before the last ui_bundle_install, say when a screenshot shows the new one broken; the open app checks it and reloads. One way only: rolling forward again takes a new install and its approval." },
  { name: "pair", method: "POST", path: "/pair", risk: null, hidden: true, summary: "Pair a phone or a node with the one-time code Bakery shows; answers the device's own token. The tailnet's only route without a token." },
  { name: "devices", method: "GET", path: "/devices", risk: null, hidden: true, summary: "The paired devices, and whether this node is reachable over the tailnet (if not, what to turn on)." },
  { name: "devicesPairCode", method: "POST", path: "/devices/pair-code", risk: null, hidden: true, summary: "Show a one-time pairing code: 8 characters, 5 minutes, one use." },
  { name: "devicesRevoke", method: "POST", path: "/devices/revoke", risk: null, hidden: true, summary: "Unpair a device: its token stops working and its connections close at once." },
  { name: "devicesRemote", method: "POST", path: "/devices/remote", risk: null, hidden: true, summary: "Make this node reachable from the operator's other devices over Tailscale, or not." },
  { name: "outsideWindow", method: "GET", path: "/outside-window", risk: null, hidden: true, summary: "The outside window's read-only token: when it was made and last used, where it is reached and what it may call." },
  { name: "outsideWindowCreate", method: "POST", path: "/outside-window/create", risk: null, hidden: true, summary: "Make the outside window's read-only token, shown this once. One at a time." },
  { name: "outsideWindowRevoke", method: "POST", path: "/outside-window/revoke", risk: null, hidden: true, summary: "Revoke the outside window's read-only token: it stops working at once." },
  { name: "desktopWs", method: "GET", path: "/desktop/ws", risk: null, hidden: true, summary: "The desktop app's link: the previews it shows, and the commands it runs on them." },
  { name: "previewSnapshot", method: "POST", path: "/preview/snapshot", risk: "read", hidden: false, summary: "What your conversation's open preview shows: its URL, title and visible text." },
  { name: "previewNavigate", method: "POST", path: "/preview/navigate", risk: "run", hidden: false, summary: "Load an http(s) URL in your conversation's open preview." },
  { name: "previewClick", method: "POST", path: "/preview/click", risk: "run", hidden: false, summary: "Click the first element a CSS selector matches in your conversation's open preview." },
  { name: "desktopScreenshot", method: "POST", path: "/desktop/screenshot", risk: "run", hidden: false, summary: "A screenshot of the Bakery window as the operator sees it, say to check a UI change: you get the image itself beside the result. The app's own page only; a Code or Preview view is never in it. Each capture is noted in your conversation." },
  { name: "previewEval", method: "POST", path: "/preview/eval", risk: "run", hidden: false, summary: "Run a JavaScript expression in your conversation's open preview and get its value as JSON. It runs in that page as the operator left it, logged-in sessions included." },
  { name: "systemGhostty", method: "GET", path: "/system/ghostty", risk: "read", hidden: false, summary: "Whether the Ghostty terminal app is available." },
  { name: "systemGhosttyOpen", method: "POST", path: "/system/ghostty/open", risk: "run", hidden: false, summary: "Open Ghostty on the operator's screen in a stream's worktree." },
  { name: "stream", method: "GET", path: "/stream/{id}", risk: "read", hidden: false, summary: "One stream's recorded facts: project, branch, worktree, lifetime." },
  { name: "seatAssign", method: "POST", path: "/seat/assign", risk: "write", hidden: false, summary: "Assign a model id (a model_options id) to a conversation's seat. While the seat's turn runs, the switch waits for that turn to end (a seat.model.pending answer) and the turn runs on; a second switch replaces it, and one back to what the seat has cancels it." },
  { name: "agentPipeGateDecide", method: "POST", path: "/agent/pipe/gate/decide", risk: null, hidden: true, summary: "Decide a native permission request." },
  { name: "streamLifetime", method: "POST", path: "/stream/lifetime", risk: "write", hidden: false, summary: "Set a stream's lifetime: living or burn." },
  { name: "streamDiscard", method: "POST", path: "/stream/discard", risk: "destructive", hidden: false, summary: "Throw away a stream's worktree and sandbox room; its history is kept." },
  { name: "filesList", method: "GET", path: "/files/list", risk: "read", hidden: false, summary: "One directory level of a stream's worktree." },
  { name: "filesRead", method: "GET", path: "/files/read", risk: "read", hidden: false, summary: "A file's full text from a stream's worktree, with its digest." },
  { name: "filesWrite", method: "POST", path: "/files/write", risk: null, hidden: true, summary: "The operator's editor save." },
  { name: "events", method: "GET", path: "/events", risk: null, hidden: true, summary: "Event WebSocket." },
  { name: "agentTurn", method: "POST", path: "/agent/turn", risk: null, hidden: true, summary: "Run one agent turn synchronously." },
  { name: "agentAttachments", method: "POST", path: "/agent/attachments", risk: null, hidden: true, summary: "Upload an attachment from the native picker." },
  { name: "attachment", method: "GET", path: "/attachment", risk: null, hidden: true, summary: "An attachment sent on a thread, as it was sent, with its media type." },
  { name: "agentTurnQueue", method: "POST", path: "/agent/turn/queue", risk: "write", hidden: false, summary: "Queue a message for a conversation's agent; it runs when the agent is free." },
  { name: "agentTurnReview", method: "POST", path: "/agent/turn/review", risk: null, hidden: true, summary: "Review all the changes one agent turn made at once: keep every one, or undo every one." },
  { name: "agentChange", method: "GET", path: "/agent/change", risk: null, hidden: true, summary: "One change an agent's turn made, before and after, read from Bakery's own copy rather than the history." },
  { name: "agentSnapshotLimit", method: "GET", path: "/agent/snapshot-limit", risk: null, hidden: true, summary: "The largest file, in MB, kept a copy of when a turn's changes are recorded: a bigger file's change can only be kept, never undone. 10 MB until the operator sets it." },
  { name: "agentSnapshotLimitSet", method: "POST", path: "/agent/snapshot-limit", risk: null, hidden: true, summary: "Set that size limit, in MB (at least 1). A turn already running keeps the limit it began with." },
  { name: "search", method: "POST", path: "/search", risk: null, hidden: true, summary: "The operator's own search: file names and text in this workspace or everywhere the Bakery has a folder, secret files included with their values masked. Folders git ignores are skipped. Never recorded." },
  { name: "searchReveal", method: "POST", path: "/search/reveal", risk: null, hidden: true, summary: "One masked line from a secret file, shown whole, for the operator alone. Never recorded." },
  { name: "agentTurnQueueSteerAll", method: "POST", path: "/agent/turn/queue/steer-all", risk: "write", hidden: false, summary: "Turn every queued follow-up into one steer for the agent's running turn." },
  { name: "agentTurnQueueWithdraw", method: "POST", path: "/agent/turn/queue/withdraw", risk: null, hidden: true, summary: "Take back a message still waiting in an agent's queue, a follow-up or a steer not yet delivered, to edit or drop it; its text comes back. Refused once it is sent, merged, or on its way into the running turn." },
  { name: "agentToolPayload", method: "GET", path: "/agent/tool-payload", risk: "read", hidden: false, summary: "The complete payload behind one native tool call record." },
  { name: "agentTurnSteer", method: "POST", path: "/agent/turn/steer", risk: "write", hidden: false, summary: "Steer a conversation's running turn with a message." },
  { name: "turnStop", method: "POST", path: "/agent/turn/cancel", risk: "write", hidden: false, summary: "Stop a conversation's running turn and pause its queue (queued messages are kept)." },
  { name: "agentDispatchResume", method: "POST", path: "/agent/dispatch/resume", risk: "write", hidden: false, summary: "Resume a paused conversation queue." },
  { name: "agentMode", method: "POST", path: "/agent/mode", risk: "write", hidden: false, summary: "Set a conversation's operating mode: manual, plan, auto_edit or auto." },
  { name: "agentTools", method: "GET", path: "/agent/tools", risk: "read", hidden: false, summary: "What a conversation's agents may use of the Bakery's tools: all, read (only those graded read) or none." },
  { name: "agentToolsSet", method: "POST", path: "/agent/tools", risk: null, hidden: true, summary: "Set what a conversation's agents may use of the Bakery's tools: all, read or none." },
  { name: "agentRuntimeConfig", method: "POST", path: "/agent/runtime/config", risk: "write", hidden: false, summary: "Set one native runtime option (model, effort, ...) a session advertises." },
  { name: "agentSelection", method: "POST", path: "/agent/selection", risk: "write", hidden: false, summary: "Set the model and effort a conversation on the Bakery's own loop runs every turn on, as its picker shows them." },
  { name: "agentActivity", method: "GET", path: "/agent/activity", risk: "read", hidden: false, summary: "What a conversation's agent is doing right now: phase, running tool, finished tools." },
  { name: "agentAccept", method: "POST", path: "/agent/accept", risk: null, hidden: true, summary: "Accept a write proposal." },
  { name: "agentAnswer", method: "POST", path: "/agent/answer", risk: null, hidden: true, summary: "Answer an agent's question." },
  { name: "agentResume", method: "POST", path: "/agent/resume", risk: null, hidden: true, summary: "Resume a turn that died with the process." },
  { name: "agentSessionChoose", method: "POST", path: "/agent/session/choose", risk: null, hidden: true, summary: "Answer a session that didn't resume: retry it, or start fresh." },
  { name: "agentReject", method: "POST", path: "/agent/reject", risk: null, hidden: true, summary: "Reject a write proposal." },
  { name: "baker", method: "GET", path: "/baker", risk: "read", hidden: false, summary: "The permanent Baker conversation." },
  { name: "threadList", method: "GET", path: "/baker/threads", risk: "read", hidden: false, summary: "The Baker's conversations: the main one and every parallel thread." },
  { name: "threadCreate", method: "POST", path: "/baker/threads", risk: "write", hidden: false, summary: "Open a new Baker thread; for another agent, a peer of its own (a workspace's orc: a worker)." },
  { name: "threadRename", method: "POST", path: "/baker/threads/rename", risk: "write", hidden: false, summary: "Rename a Baker thread." },
  { name: "threadArchive", method: "POST", path: "/baker/threads/archive", risk: "write", hidden: false, summary: "Archive or restore a Baker thread." },
  { name: "threadRead", method: "GET", path: "/baker/threads/read", risk: "read", hidden: false, summary: "A conversation's work: its runtime, model, effort and mode, its latest messages, and its last finished turn's result (status, stop reason, tokens, acceptance)." },
  { name: "threadWait", method: "POST", path: "/baker/threads/wait", risk: "read", hidden: false, summary: "Wait until a conversation has nothing left to run, up to 50 s, then return what thread_read returns, plus done:false when it is still working." },
  { name: "threadBoard", method: "GET", path: "/baker/threads/board", risk: "read", hidden: false, summary: "Every thread you look after, in one call (an orc: itself and its workers; the Baker: everything, its conversations first, then every workspace, worker and project thread in use; another agent: itself and the peers it opened): each one's state and last line, its branch, and from git, against main (a workspace's worker against its workspace's branch too), the commits not in yet, whether they are merged or it made none, and whether the rest merges cleanly." },
  { name: "bakerAssign", method: "POST", path: "/baker/assign", risk: "run", hidden: false, summary: "Start a legacy Baker branch job on a repository." },
  { name: "bakerWork", method: "GET", path: "/baker/work", risk: "read", hidden: false, summary: "Legacy Baker branch jobs." },
  { name: "bakerWorkDetail", method: "GET", path: "/baker/work/detail", risk: "read", hidden: false, summary: "One legacy Baker branch job." },
  { name: "bakerPush", method: "POST", path: "/baker/push", risk: "destructive", hidden: false, summary: "Push a Baker branch to its remote." },
  { name: "bakerPr", method: "POST", path: "/baker/pr", risk: "destructive", hidden: false, summary: "Open a pull request for a Baker branch." },
  { name: "bakerMerge", method: "POST", path: "/baker/merge", risk: "destructive", hidden: false, summary: "Merge a Baker branch." },
  { name: "bakerCapabilities", method: "GET", path: "/baker/capabilities", risk: "read", hidden: false, summary: "What the Baker can do: its operations and their summaries." },
  { name: "taskCreate", method: "POST", path: "/task/create", risk: "write", hidden: false, summary: "Create a task: its durable identity and completion contract." },
  { name: "taskStart", method: "POST", path: "/task/start", risk: "run", hidden: false, summary: "Materialize a task's workspace and start its agent." },
  { name: "taskMessage", method: "POST", path: "/task/message", risk: "write", hidden: false, summary: "Queue a steering message in a task's inbox." },
  { name: "taskCancel", method: "POST", path: "/task/cancel", risk: "write", hidden: false, summary: "Cancel a task." },
  { name: "taskRetire", method: "POST", path: "/task/retire", risk: "destructive", hidden: false, summary: "Retire a task and release its worktree and sandbox room." },
  { name: "taskList", method: "GET", path: "/task/list", risk: "read", hidden: false, summary: "The tasks on one Project: status, contract, warnings." },
  { name: "taskReview", method: "GET", path: "/task/review", risk: "read", hidden: false, summary: "A task's review: its diff against the baseline and its checks." },
  { name: "taskSignoff", method: "POST", path: "/task/signoff", risk: null, hidden: true, summary: "Sign off a task's review." },
  { name: "taskPush", method: "POST", path: "/task/push", risk: "destructive", hidden: false, summary: "Push a task's branch to its remote." },
  { name: "taskPr", method: "POST", path: "/task/pr", risk: "destructive", hidden: false, summary: "Open a pull request for a task's branch." },
  { name: "taskIntegrate", method: "POST", path: "/task/integrate", risk: "destructive", hidden: false, summary: "Merge a signed-off task into its project's branch." },
  { name: "operatorSnapshot", method: "GET", path: "/operator/snapshot", risk: "read", hidden: false, summary: "One snapshot of the node: streams, gates, agents." },
  { name: "authStatus", method: "GET", path: "/auth/status", risk: null, hidden: true, summary: "Provider credential status." },
  { name: "authCliStatus", method: "GET", path: "/auth/cli-status", risk: null, hidden: true, summary: "CLI login status." },
  { name: "authClaudeNativeStatus", method: "GET", path: "/auth/claude-native/status", risk: null, hidden: true, summary: "Claude login status." },
  { name: "authApiKey", method: "POST", path: "/auth/api-key", risk: null, hidden: true, summary: "Set an API key." },
  { name: "authClear", method: "POST", path: "/auth/clear", risk: null, hidden: true, summary: "Clear a credential." },
  { name: "authModel", method: "POST", path: "/auth/model", risk: null, hidden: true, summary: "Set a provider model." },
  { name: "authXaiDeviceStart", method: "POST", path: "/auth/xai/device/start", risk: null, hidden: true, summary: "Start an xAI device login." },
  { name: "authXaiDevicePoll", method: "POST", path: "/auth/xai/device/poll", risk: null, hidden: true, summary: "Poll an xAI device login." },
  { name: "authGeminiImportCli", method: "POST", path: "/auth/gemini/import-cli", risk: null, hidden: true, summary: "Import the Gemini CLI login." },
  { name: "authClaudeCodeLoginStart", method: "POST", path: "/auth/claude-code/login/start", risk: null, hidden: true, summary: "Start a Claude Code login." },
  { name: "authClaudeCodeLoginComplete", method: "POST", path: "/auth/claude-code/login/complete", risk: null, hidden: true, summary: "Complete a Claude Code login." },
  { name: "authPlugSecret", method: "POST", path: "/auth/plug-secret", risk: null, hidden: true, summary: "Set a plug secret." },
  { name: "authPlugSecretClear", method: "POST", path: "/auth/plug-secret/clear", risk: null, hidden: true, summary: "Clear a plug secret." },
  { name: "providers", method: "GET", path: "/providers", risk: "read", hidden: false, summary: "The configured model providers and runtime plugs." },
  { name: "providersAcpPlug", method: "POST", path: "/providers/acp-plug", risk: null, hidden: true, summary: "Add an ACP runtime plug." },
  { name: "providersAcpPlugUpdate", method: "POST", path: "/providers/acp-plug/update", risk: null, hidden: true, summary: "Change an ACP runtime plug." },
  { name: "providersAcpPlugDelete", method: "POST", path: "/providers/acp-plug/delete", risk: null, hidden: true, summary: "Remove an ACP runtime plug." },
  { name: "providersOpenaiCompatPlug", method: "POST", path: "/providers/openai-compat-plug", risk: null, hidden: true, summary: "Add an OpenAI-compatible plug." },
  { name: "providersOpenaiCompatPlugUpdate", method: "POST", path: "/providers/openai-compat-plug/update", risk: null, hidden: true, summary: "Change an OpenAI-compatible plug." },
  { name: "providersOpenaiCompatPlugDelete", method: "POST", path: "/providers/openai-compat-plug/delete", risk: null, hidden: true, summary: "Remove an OpenAI-compatible plug." },
  { name: "studioState", method: "GET", path: "/studio/state", risk: "read", hidden: false, summary: "The studio's whole state: projects, conversations, seats, models." },
  { name: "modelsCatalog", method: "GET", path: "/models/catalog", risk: "read", hidden: false, summary: "One provider's live models with their effort levels and context windows." },
  { name: "accountsCapabilities", method: "GET", path: "/accounts/capabilities", risk: "read", hidden: false, summary: "What one account's runtime can do: live catalog, settings, auth." },
  { name: "mcpTools", method: "GET", path: "/mcp/tools", risk: null, hidden: true, summary: "MCP tool list." },
  { name: "mcpCall", method: "POST", path: "/mcp/call", risk: null, hidden: true, summary: "MCP tool call." },
  { name: "mcp", method: "POST", path: "/mcp", risk: null, hidden: true, summary: "The outside window: read-only MCP over HTTP, for its read-only token." },
  { name: "eventsTicket", method: "POST", path: "/events/ticket", risk: null, hidden: true, summary: "Event WebSocket ticket." },
];
