//! The node's HTTP API as one catalog: every route `api_routes()` serves,
//! with the input it takes and one line on what it does. The Baker's tools
//! are generated from it (`api_tools`), so whatever the operator's UI can
//! call, the Baker can call too: through the same handler, after the gate,
//! minus the operator-only routes.
//!
//! Two layers, after vendo's extraction + overrides. [`ROUTES`] states facts
//! (method, path, input type, summary), and a test holds it to `main.rs`'s
//! router in both directions. `api_overrides.json` is policy: a tool's name,
//! its risk grade, whether it sits on the Baker's menu, whether it asks the
//! operator in every mode, and which routes the Baker never sees, at any
//! sandbox scope. Overrides win field by field.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum Method {
    #[serde(rename = "GET")]
    Get,
    #[serde(rename = "POST")]
    Post,
}

impl Method {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
        }
    }
}

/// How much a call can change, which decides whether the gate asks first.
/// `read` changes nothing; `write` changes Bakery state; `run` starts
/// programs or agent work; `destructive` is hard to undo or leaves the
/// machine (discard, retire, push, merge).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Risk {
    Read,
    Write,
    Run,
    Destructive,
}

impl Risk {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Risk::Read => "read",
            Risk::Write => "write",
            Risk::Run => "run",
            Risk::Destructive => "destructive",
        }
    }
}

pub(crate) struct Route {
    pub(crate) method: Method,
    pub(crate) path: &'static str,
    /// The query (GET) or body (POST) schema; `None` takes nothing.
    pub(crate) input: SchemaFn,
    pub(crate) summary: &'static str,
    /// What it answers with, for the generated UI client; `None` until a
    /// row says ([`Route::returns`]).
    pub(crate) output: SchemaFn,
}

/// A route's schema, from whichever generator asks: one that inlines every
/// subschema for the models' tools, one that names shared types for the
/// generated UI client.
pub(crate) type SchemaFn = fn(&mut schemars::SchemaGenerator) -> Option<Value>;

impl Route {
    /// The tool input this route takes, path parameters included.
    pub(crate) fn input(&self) -> Value {
        tool_input(self)
    }

    /// What this route answers with: [`schema`] of its response type, or
    /// [`untyped`] for a handler that returns plain JSON.
    const fn returns(mut self, output: SchemaFn) -> Self {
        self.output = output;
        self
    }
}

const fn get(path: &'static str, input: SchemaFn, summary: &'static str) -> Route {
    Route { method: Method::Get, path, input, summary, output: none }
}

const fn post(path: &'static str, input: SchemaFn, summary: &'static str) -> Route {
    Route { method: Method::Post, path, input, summary, output: none }
}

fn none(_: &mut schemars::SchemaGenerator) -> Option<Value> {
    None
}

/// A type's JSON schema, as the generator asking wants it.
pub(crate) fn schema<T: schemars::JsonSchema>(g: &mut schemars::SchemaGenerator) -> Option<Value> {
    Some(serde_json::to_value(g.subschema_for::<T>()).unwrap_or_default())
}

/// A request type's schema; [`tool_input`] reduces it for the models.
fn input<T: schemars::JsonSchema>(g: &mut schemars::SchemaGenerator) -> Option<Value> {
    schema::<T>(g)
}

/// The shape of a Heart event (`bakery_core::Event`, which carries no
/// schema of its own), for the schemas of responses that hold one.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) struct EventShape {
    stream_id: String,
    seq: u64,
    content_hash: String,
    prev_hash: Option<String>,
    actor: String,
    ts: String,
    causality: Vec<String>,
    layer: Option<String>,
    sensitivity: String,
    family: String,
    /// The event's payload, itself JSON text.
    payload: String,
    event_hash: String,
}

/// The shape of `agent_control::Queued`: the queued message's event, and
/// `started`, the turn's own event, when an idle agent's turn began at once.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) struct QueuedShape {
    #[serde(flatten)]
    event: EventShape,
    started: Option<EventShape>,
}

/// The shape of `bakery_agent::cli_tools::CliAccountStatus`.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) struct CliAccountShape {
    installed: bool,
    linked: bool,
}

/// The shape of `bakery_agent::cli_tools::LoginCommand`.
#[derive(schemars::JsonSchema)]
#[allow(dead_code)]
pub(crate) enum LoginCommandShape {
    #[serde(rename = "codex-login")]
    Codex,
    #[serde(rename = "cursor-login")]
    Cursor,
    #[serde(rename = "claude-native-login")]
    ClaudeNative,
}

/// The marker for a handler that answers with plain, untyped JSON.
pub(crate) const UNTYPED: &str = "x-bakery-untyped";

pub(crate) fn untyped(_: &mut schemars::SchemaGenerator) -> Option<Value> {
    Some(json!({ UNTYPED: true }))
}

use crate::agents::{ArchiveReq, RenameConversationReq, StreamReq};
use crate::jobs::{JobListQuery, JobOutputQuery, JobStartReq, JobStopReq};
use crate::baker_threads::{
    ArchiveThreadReq, CreateThreadReq, ListThreadsQuery, RenameThreadReq, ThreadReadQuery, ThreadWaitReq,
};
use crate::routes::agent::{
    AgentActivityQuery, AgentModeReq, AgentQueueReq, AgentTurnCancelReq, NativeConfigReq, SelectionReq,
};
use crate::routes::baker::{
    BakerAssignReq, BakerBranchReq, BakerMergeReq, BakerPrReq, BakerWorkDetailQuery,
    CapabilitiesQuery,
};
use crate::routes::env::{AttachReq, EnvQuery, ProvisionReq};
use crate::routes::lifecycle::{DiscardReq, LifetimeReq};
use crate::routes::memory::{MemoryWriteReq, StreamQuery};
use crate::routes::operator::HeartQuery;
use crate::routes::pipe::ToolPayloadQuery;
use crate::routes::project::{FilesQuery, InspectReq, OpenReq, OpenWorktreeReq};
use crate::routes::project_catalog::{
    AttachSocketReq, CreateChatReq, CreateProjectReq, DetachSocketReq, OpenRootReq, PinProjectReq,
    RenameProjectReq, RetireProjectReq,
};
use crate::routes::seats::SeatAssignReq;
use crate::routes::studio::{AccountCapabilitiesQuery, ModelCatalogQuery};
use crate::routes::system::GhosttyOpenReq;
use crate::routes::terminal::{CloseReq, ListQuery, RenameReq};
use crate::task_review::TaskReviewQuery;
use crate::task_ship::TaskShipReq;
use crate::tasks::{
    TaskCancelReq, TaskCreateReq, TaskListQuery, TaskMessageReq, TaskRetireReq, TaskStartReq,
};

/// Every route the node serves, in `api_routes()` order. Hidden routes need
/// no input schema: nothing generated ever calls them.
pub(crate) const ROUTES: &[Route] = &[
    get("/health", none, "Whether the node is up.").returns(schema::<String>),
    get("/node", none, "This node's identity and policy.").returns(schema::<crate::routes::operator::NodeInfo>),
    get("/versions", none, "What this node runs: Bakery build, Node.js, the Claude adapter and engine, each live session's runtime.").returns(schema::<crate::versions::Report>),
    get("/agents", none, "Every agent and its runtime, model, state and conversation.").returns(schema::<crate::agents::AgentsView>),
    get("/agents/tool-policy", none, "The operator's node-wide policy per native tool kind: always, ask or never.").returns(schema::<Vec<crate::agents::KindPolicy>>),
    post("/agents/tool-policy", input::<crate::agents::SetPolicy>, "Set the node-wide native tool policy.").returns(schema::<Vec<crate::agents::KindPolicy>>),
    get("/agents/always-allowed", none, "Every \"Always allow\" an approval card made: a kind of action, allowed without asking in one project, in the Baker's conversations, or in one chat.").returns(schema::<Vec<crate::pipe::GrantView>>),
    post("/agents/always-allowed/remove", input::<crate::pipe::AlwaysGrant>, "Take back one \"Always allow\": that kind asks again there.").returns(schema::<Vec<crate::pipe::GrantView>>),
    get("/agents/peer-limit", none, "The most open peers agents may open in one project, or one workspace.").returns(schema::<crate::agents::PeerLimit>),
    post("/agents/peer-limit", input::<crate::agents::PeerLimit>, "Set the most open peers agents may open in one project, or one workspace.").returns(schema::<crate::agents::PeerLimit>),
    post("/chat/create", input::<CreateChatReq>, "Start a new chat in its own folder; with model_id and instruction it starts working right away.").returns(schema::<crate::state::ActiveContext>),
    post("/chat/make-project", input::<crate::routes::project_catalog::MakeProjectReq>, "Make a chat a project: its folder moves into Bakery's projects folder, or a folder chosen, as a Git repository, and the conversation carries on there.").returns(schema::<crate::routes::project_catalog::MadeProject>),
    post("/conversation/archive", input::<ArchiveReq>, "Archive (archived:true) or restore any conversation: a chat, a project socket or a Baker thread.").returns(untyped),
    post("/conversation/rename", input::<RenameConversationReq>, "Rename any conversation: a chat, a project's thread or a Baker thread; the main Baker keeps its name.").returns(untyped),
    post("/conversation/restart", input::<StreamReq>, "Stop a conversation's idle runtime; its next turn starts a fresh one with the whole history.").returns(untyped),
    get("/agents/storage", none, "Runtime home storage.").returns(schema::<crate::agents::StorageView>),
    get("/agents/storage/list", input::<crate::agents::PathQuery>, "List a runtime home folder.").returns(schema::<Vec<crate::agents::Entry>>),
    get("/agents/storage/file", input::<crate::agents::PathQuery>, "Read a runtime home file.").returns(schema::<crate::agents::FileView>),
    post("/agents/storage/delete", input::<crate::agents::PathQuery>, "Delete runtime home storage.").returns(untyped),
    get("/machine-access", none, "Which folders, programs and variables agents may reach, and what stays denied under any grant.").returns(schema::<crate::machine_access::View>),
    post("/machine-access", input::<crate::machine_access::MachineAccess>, "Set machine access.").returns(schema::<crate::machine_access::Saved>),
    post("/machine-access/check", input::<crate::machine_access::CheckReq>, "Probe the sandbox.").returns(schema::<crate::machine_access::CheckResult>),
    post("/machine-access/requests/decide", input::<crate::machine_access::DecideAccess>, "Decide a folder access request.").returns(schema::<crate::api_catalog::EventShape>),
    post("/machine-access/reach", input::<crate::machine_access::ReachReq>, "Set one agent's reach on the scope dial.").returns(schema::<crate::machine_access::ReachSet>),
    post("/machine-access/containers", input::<crate::machine_access::ContainersReq>, "Turn one agent's containers switch on or off.").returns(schema::<crate::machine_access::ContainersSet>),
    post("/machine-access/seat-size", input::<crate::machine_access::SeatSizeReq>, "Set one container seat's CPUs and memory, or return it to the default sized from this Mac.").returns(schema::<crate::machine_access::SeatSizeSet>),
    post("/machine-access/claude-container/sign-in", none, "Sign Claude in for container seats, in a terminal pane.").returns(schema::<crate::terminals::TerminalInfo>),
    get("/containers/seat-image", none, "Whether the seat image is built from this node's recipe: missing, stale or current.").returns(schema::<crate::seat_container::SeatImage>),
    post("/containers/seat-image/build", none, "Build the seat image from this node's recipe, in a terminal pane the operator sees.").returns(schema::<crate::terminals::TerminalInfo>),
    get("/containers", input::<crate::containers::ListQuery>, "Containers, images, volumes, networks or compose projects on Apple container or Docker, as rows both backends share; a container Bakery started names its role, conversation and seat.").returns(schema::<crate::containers::Listing>),
    get("/containers/inspect", input::<crate::containers::ItemQuery>, "One container, image, volume, network or compose project, as the backend describes it.").returns(untyped),
    get("/containers/logs", input::<crate::containers::LogsQuery>, "A container's last log lines.").returns(untyped),
    get("/containers/stats", input::<crate::containers::BackendQuery>, "CPU, memory and I/O of every running container on a backend.").returns(untyped),
    post("/containers/start", input::<crate::containers::ContainerReq>, "Start a stopped container.").returns(untyped),
    post("/containers/stop", input::<crate::containers::ContainerReq>, "Stop a running container.").returns(untyped),
    post("/containers/restart", input::<crate::containers::ItemReq>, "Restart a container, or a whole compose project (kind compose): it stops and starts again, its files kept. The Bakery's own are refused while in use and otherwise start fresh: an agent's sandbox with its next turn, a conversation's editor the next time Code opens.").returns(untyped),
    post("/containers/remove", input::<crate::containers::ItemReq>, "Remove a stopped container, an image, a volume or a network, or take a compose project down.").returns(untyped),
    post("/containers/prune", input::<crate::containers::KindReq>, "Remove every unused container, image, volume or network on a backend.").returns(untyped),
    post("/containers/compose/up", input::<crate::containers::ComposeUpReq>, "Start a Docker compose project from its file, detached.").returns(untyped),
    post("/containers/run", input::<crate::containers::RunReq>, "Start a container from an image, detached, its ports on 127.0.0.1 only; for an agent, what it can't reach stays out of its mounts.").returns(schema::<crate::containers::Ran>),
    post("/code/open", input::<crate::views::CodeReq>, "Open a conversation's worktree in the operator's editor (openvscode-server in a container of its own) at a 127.0.0.1 URL that carries its connection token.").returns(schema::<crate::views::ViewUrl>),
    post("/code/close", input::<crate::views::CodeReq>, "Stop a conversation's editor.").returns(schema::<crate::views::Closed>),
    post("/code/appearance", input::<crate::editor_style::AppearanceReq>, "The scheme the app shows now (its colors as the app resolves them, light or dark, its code font), for every Code editor: open ones follow at once. The app sends it when its scheme or the Mac's light or dark mode changes.").returns(untyped),
    post("/code/goto", input::<crate::views::GotoReq>, "Open an edit row's file in its conversation's editor at the first line the edit changed, and say whether the file has changed since.").returns(schema::<crate::views::Goto>),
    post("/preview/open", input::<crate::views::PreviewReq>, "A 127.0.0.1 URL for the dev server a conversation runs on a port, in its container or on this Mac.").returns(schema::<crate::views::ViewUrl>),
    get("/model-proxy", none, "The model proxy (CLIProxyAPI): whether it's installed and running, and the hosts its upstream calls may reach.").returns(schema::<crate::model_proxy::ProxyStatus>),
    post("/model-proxy/install", none, "Install the pinned model proxy release, its sha256 checked, and start it.").returns(schema::<crate::model_proxy::ProxyStatus>),
    post("/model-proxy/start", none, "Start the installed model proxy again, after fixing what stopped it.").returns(schema::<crate::model_proxy::ProxyStatus>),
    get("/model-proxy/keys", none, "Which providers the model proxy holds API keys for, and how many; never a key.").returns(schema::<crate::model_proxy::ModelKeys>),
    post("/model-proxy/keys", input::<crate::model_proxy::AddKey>, "Give the model proxy an API key for a provider.").returns(schema::<crate::model_proxy::ModelKeys>),
    post("/model-proxy/keys/remove", input::<crate::model_proxy::RemoveKey>, "Take an API key back from the model proxy, by its fingerprint.").returns(schema::<crate::model_proxy::ModelKeys>),
    get("/metrics", none, "The model proxy's numbers in Prometheus's text format: calls, tokens and latency by agent, provider and model, and calls another model answered.").returns(untyped),
    get("/agents/map", none, "Which conversation started which: the agent map.").returns(schema::<crate::agents::AgentMap>),
    post("/versions/check", none, "Ask npm for the newest releases of the node's runtimes.").returns(schema::<crate::versions::Report>),
    get("/control/bakery", none, "The deployed Bakery build and the last deploy, restart or rollback.").returns(schema::<crate::control::ControlStatus>),
    post("/control/bakery", input::<crate::control::ControlReq>, "Deploy, deploy only the UI (the open app reloads, nothing restarts), restart or roll back Bakery.").returns(schema::<crate::control::ControlStatus>),
    get("/mac/amphetamine", none, "Amphetamine on this Mac: its session, the time left, closed-display mode.").returns(schema::<crate::this_mac::Amphetamine>),
    post("/mac/amphetamine", input::<crate::this_mac::AmphetamineReq>, "Start or end an Amphetamine session, or turn its closed-display mode on or off, through Amphetamine's own scripting.").returns(schema::<crate::this_mac::Amphetamine>),
    post("/mac/fans/open", none, "Open Macs Fan Control, which has no other way in.").returns(schema::<()>),
    get("/mac/meters", none, "This Mac's disk, memory, CPU and heat, each with its level (ok, warn, bad), and the last ten minutes of CPU and memory.").returns(schema::<crate::this_mac::Meters>),
    get("/mac/heat", none, "This Mac's thermal state, hottest sensor and fans, and whether agents' new turns wait for it to cool.").returns(schema::<crate::this_mac::Heat>),
    get("/disk", none, "Where this Mac's disk goes for the Bakery: free space, each thread's build folders, every image, container and volume on Docker and Apple container with the project that uses it, Colima's disk and the Bakery's own data.").returns(schema::<crate::disk::DiskReport>),
    post("/disk/clean", input::<crate::disk::CleanReq>, "One cleanup the operator picked and confirmed: a listed build folder, the images no container uses (never the Bakery's own), stopped containers, Docker's build cache, or the volumes they ticked by name.").returns(schema::<crate::disk::Cleaned>),
    get("/engine", none, "What this Mac runs for the Bakery: Apple container, Tailscale and its machines, Colima, Homebrew's services, cloudflared, Supabase's local stacks; for Colima's default VM, what it uses inside, per container and of this Mac, its peak, a suggested size, the size waiting for its next start and its idle stop.").returns(schema::<crate::engine::EngineRoom>),
    post("/engine/colima/idle-stop", input::<crate::engine::IdleStopReq>, "Turn Colima's idle stop on or off: its default VM stops once nothing has used Docker for a while, and a Docker action starts it again.").returns(schema::<bool>),
    post("/engine/docker-default", input::<crate::engine::DockerDefaultReq>, "Pick the Bakery's Docker engine for projects that pick none: colima or orbstack (installed first); it answers what the switch means for images and volumes already there.").returns(schema::<crate::engine::DockerDefault>),
    post("/engine/orbstack/install", none, "The operator's yes to installing OrbStack with Homebrew; it installs in the background and the app hears how it went.").returns(schema::<String>),
    post("/engine/colima/size", input::<crate::engine::ColimaSizeReq>, "Pick the memory (and CPUs) Colima's default VM starts with next, or take a size back before it applies.").returns(schema::<Option<crate::engine::ColimaSize>>),
    post("/project/service/copy", input::<crate::services::ServiceCopyReq>, "Start a separate copy of one of this project's services (redis, postgres) for destructive work, with its own data and port, reachable by this thread alone. Stop it with service_copy_stop when done.").returns(schema::<crate::services::ServiceCopy>),
    post("/project/service/copy/stop", input::<crate::services::ServiceStopReq>, "Stop this thread's copy of a service, by its port; its data goes with it.").returns(schema::<Vec<crate::services::ServiceCopy>>),
    get("/files/read/asks", input::<crate::secret_reads::SecretFilesQuery>, "A project's secret-named files: the operator's answers on which its builds may read, and the files its rooms were refused that wait on them.").returns(schema::<crate::secret_reads::SecretFiles>),
    post("/files/read/decide", input::<crate::secret_reads::FileReadDecide>, "The operator's answer for a secret-named file a project's build needs: allow it (readable, never writable, in its rooms), deny it, or forget the answer. A private key takes sure: true.").returns(schema::<crate::secret_reads::SecretFiles>),
    post("/control/restart/now", none, "A restart waiting for running turns goes ahead at once; the turns it cuts off carry on after it.").returns(schema::<Option<crate::service::RestartWait>>),
    post("/client/log", input::<crate::control::ClientLog>, "Client log line.").returns(untyped),
    post("/policy/write-mode", input::<crate::routes::agent::WriteModeReq>, "Set the node-wide write mode.").returns(schema::<crate::routes::operator::NodeInfo>),
    post("/project/inspect", input::<InspectReq>, "What a folder is: git repo, branch, worktrees, stacks. Changes nothing.").returns(schema::<crate::routes::project::InspectResp>),
    post("/project/open", input::<OpenReq>, "Open a folder as a project socket (attach a branch or create a worktree).").returns(schema::<crate::routes::project::OpenResp>),
    post("/project/create", input::<CreateProjectReq>, "Create a Project: its own folder inside Bakery (root {mode:\"bakery\"}) or an existing folder ({mode:\"attach\",path}).").returns(schema::<crate::routes::project_catalog::CreateProjectResp>),
    post("/project/rename", input::<RenameProjectReq>, "Rename a Project; its project_id never changes.").returns(untyped),
    get("/project/control", input::<crate::control_plane::ControlQuery>, "A project's control plane, read from its files now: per folder what it's built with, its direct dependencies and tools; the MCP servers it defines plus the Bakery's own; its skills, once each; its instruction files; and the Bakery's own setup for it with what its files suggest. Every item names its source file.").returns(schema::<crate::control_plane::ControlPlane>),
    post("/project/setup", input::<crate::control_plane::SetupReq>, "Set the Bakery's own setup for a project (toolchain manager, task runner, containers, secrets); each agent's next turn uses it.").returns(schema::<crate::node::ProjectStacks>),
    get("/network/hosts", input::<crate::host_list::HostsQuery>, "A project's network list: the hosts its threads' sandboxes reach (the defaults, the hosts its files point to, and the operator's answers), each with where it comes from, and the hosts waiting on the operator.").returns(schema::<crate::host_list::Hosts>),
    post("/network/host/decide", input::<crate::host_list::Decide>, "The operator's answer to a host for a project: allow, deny, or null to forget it so it asks again. It resolves each open ask it covers, in the thread that asked.").returns(schema::<crate::host_list::Hosts>),
    post("/project/toolchain/keep", input::<crate::mac_sandbox::KeepReq>, "Keep one of a Mac sandbox project's toolchain additions for good: the operator's own FVM, mise or gems gets it, for every project.").returns(schema::<Vec<crate::mac_sandbox::Addition>>),
    post("/project/toolchain/request", input::<crate::mac_sandbox::InstallReq>, "Ask the operator to install a toolchain or version into this Mac sandbox project's own copies: a Flutter version (tool flutter), a mise tool's version (tool mise, name node), or a gem. Their answer, and how the install went, come back to this conversation.").returns(schema::<String>),
    post("/project/toolchain/decide", input::<crate::mac_sandbox::InstallDecide>, "The operator's answer to an install ask: yes installs it into the project's own copies and tells the thread how it went; no tells the thread so.").returns(schema::<Vec<crate::mac_sandbox::Install>>),
    get("/thread/door", input::<crate::doors::DoorQuery>, "A thread's door on this Mac, as a proxy URL for the app's Preview of it: the Preview then reaches only what the thread's project list allows.").returns(schema::<crate::doors::DoorUrl>),
    get("/thread/toolchains", input::<crate::doctor::ToolchainsQuery>, "The toolchains a thread's folder pins (mise's tools, Rust, FVM's Flutter, Xcode) beside what its own room runs, asked inside that room.").returns(schema::<crate::doctor::Toolchains>),
    get("/thread/iphone", input::<crate::iphone::IphoneBuildQuery>, "Whether a thread has an unsigned iPhone build for Build to my iPhone, looked for where the button looks.").returns(schema::<crate::iphone::IphoneBuild>),
    post("/thread/iphone/run", input::<crate::iphone::IphoneRunReq>, "Run on my iPhone (as me): types flutter run for the thread's app, on the paired iPhone, into a new terminal of the operator's own in its worktree. Nothing runs until the operator presses Enter; then it runs as them.").returns(schema::<crate::iphone::IphoneRunPrepared>),
    post("/thread/iphone/install", input::<crate::iphone::IphoneInstallReq>, "Build to my iPhone: the thread's newest unsigned iOS build (flutter build ios --no-codesign), signed with the operator's own certificate for their provisioning profile and installed on their paired iPhone.").returns(schema::<crate::iphone::IphoneInstalled>),
    post("/project/pin", input::<PinProjectReq>, "Pin or unpin a Project in the sidebar.").returns(untyped),
    post("/project/retire", input::<RetireProjectReq>, "Remove a Project from Bakery's lists; never deletes its files, roots, sockets, memory or history.").returns(untyped),
    post("/project/root/open", input::<OpenRootReq>, "Open a new directory-root socket on an existing Project.").returns(schema::<crate::state::ActiveContext>),
    post("/project/socket/attach", input::<AttachSocketReq>, "Attach an existing socket to a Project.").returns(untyped),
    post("/project/socket/detach", input::<DetachSocketReq>, "Detach a socket from its Project.").returns(untyped),
    get("/project/rollup", input::<crate::routes::rollup::RollupQuery>, "A Project at a glance: each repo's open pull requests (checks pass/fail/pending, review) and latest CI run per branch and workflow (gh), and each worktree's branch, lines added and removed since it forked, and whether it has uncommitted changes.").returns(schema::<crate::routes::rollup::ProjectRollup>),
    get("/streams", none, "Every conversation (socket) this node knows, most recently active first.").returns(schema::<Vec<crate::state::StreamRef>>),
    get("/gates", none, "Every pending write proposal across the node, oldest first.").returns(schema::<Vec<crate::routes::operator::Gate>>),
    get("/heart", input::<HeartQuery>, "Event-log statistics for the node or one stream.").returns(schema::<crate::routes::operator::HeartStats>),
    get("/memory", input::<StreamQuery>, "A stream's durable memory entries.").returns(schema::<crate::routes::memory::MemoryView>),
    post("/memory/write", input::<MemoryWriteReq>, "Write a durable memory note on a stream.").returns(schema::<crate::api_catalog::EventShape>),
    get("/toolkit", input::<crate::toolkit::ToolkitQuery>, "The Bakery's toolkit: its skills, MCP servers, hooks, plugins and instructions, Bakery-wide and per project, each with how every runtime gets it.").returns(schema::<crate::toolkit::Toolkit>),
    post("/toolkit", input::<crate::toolkit::SetReq>, "Add a toolkit item, or change one (with its id).").returns(schema::<crate::toolkit::ToolkitView>),
    post("/toolkit/remove", input::<crate::toolkit::IdReq>, "Remove a toolkit item.").returns(schema::<crate::toolkit::Toolkit>),
    post("/toolkit/enable", input::<crate::toolkit::EnableReq>, "Turn a toolkit item on or off.").returns(schema::<crate::toolkit::ToolkitView>),
    get("/toolkit/port", input::<crate::toolkit_port::PortQuery>, "The VS Code tool port: what the operator's Claude Code setup (~/.claude and ~/.claude.json) holds, read now: its instructions and rules, skills, MCP servers, hooks and plugins, and what has no Toolkit kind yet; each with what it becomes, the keys left out (by name), what changes on the way, and the Toolkit item it would change. The operator's only.").returns(schema::<crate::toolkit_port::Port>),
    post("/toolkit/port", input::<crate::toolkit_port::ImportReq>, "Bring the ticked items of the operator's Claude Code setup into the Toolkit, Bakery-wide or for one project; one already there is changed. No key or secret comes across. The operator's only.").returns(schema::<crate::toolkit_port::ImportResult>),
    get("/learning", none, "The memory's learning loop: its settings (each part on or off, every trigger, time and limit, the models, the projects in it, the pause), what holds parts back now, the models it may ask, when the nightly run next starts, and each part's last run.").returns(schema::<crate::learning::Learning>),
    post("/learning", input::<crate::learning::LoopSettings>, "Change any of the learning loop's settings: what is sent is laid over them.").returns(schema::<crate::learning::Learning>),
    post("/learning/run", input::<crate::learning::RunReq>, "Run one part of the learning loop now (a thread-end review names its thread), whatever the pause and the time; it runs on behind the answer.").returns(schema::<crate::learning::LearningRun>),
    get("/learning/runs", input::<crate::learning::RunsQuery>, "The learning loop's runs, the newest first: what each did, the notes it touched, the proposals it made and the model calls it spent.").returns(schema::<crate::learning::LearningRuns>),
    post("/memory/search", input::<crate::memory_index::MemorySearchReq>, "Search the Bakery's memory: the notes you see (your thread's, your project's and the Bakery's) that match, best first, a line each with how much evidence it rests on and whether it is stale. Read the ones you need whole with memory_get.").returns(schema::<crate::memory_index::MemoryFound>),
    post("/memory/get", input::<crate::memory_index::MemoryGetReq>, "Read notes whole, by id, within a token budget: title, summary, body, why, how to apply, links, evidence, versions and how each is used.").returns(schema::<crate::memory_index::MemoryGot>),
    post("/memory/related", input::<crate::memory_index::MemoryRelatedReq>, "The notes linked to one, either way, one or two links away.").returns(schema::<crate::memory_index::MemoryRelated>),
    get("/memory/notes", input::<crate::memory_index::MemoryNotesQuery>, "Every kept note, newest first, with how each is used and whether it is stale: the wiki's pages.").returns(schema::<crate::memory_index::MemoryNotes>),
    get("/memory/note", input::<crate::memory_index::MemoryNoteQuery>, "One note's page: the note, how it is used, and every version.").returns(schema::<crate::memory_index::NotePage>),
    post("/memory/propose", input::<crate::memory_review::ProposeReq>, "Propose a note for the Bakery's memory: only what will still matter in six months and is hard to find again from the code, git or the thread's history, resting on evidence. Never propose a secret; task progress, a todo or a log of finished work; what the code or its docs already say; the story of a one-off task; a setup failure the operator can fix (a missing tool, an unset credential); a claim that a tool or feature doesn't work; an error that went away on a retry (the retry is the lesson); or attempts that never worked (only a fix you know works). It waits for the operator, who keeps it, edits it or declines it; a repeat of a kept, waiting or declined note adds nothing.").returns(schema::<crate::memory_review::Proposed>),
    post("/memory/rate", input::<crate::memory_review::RateReq>, "Say whether a note you used helped or hurt the work; your latest word on a note counts.").returns(schema::<crate::memory_review::Rated>),
    get("/memory/proposals", input::<crate::memory_review::ProposalsQuery>, "Notes agents proposed: those waiting for the operator (or kept, or declined), the oldest first.").returns(schema::<crate::memory_review::MemoryProposals>),
    post("/memory/review", input::<crate::memory_review::ReviewReq>, "Keep a proposed note, as it is or edited (a wider scope too), or decline it; a declined one never comes back.").returns(schema::<crate::memory_review::Reviewed>),
    post("/memory/keep-all", input::<crate::memory_review::KeepAllReq>, "Keep every waiting proposal at once, as it is (or those named).").returns(schema::<crate::memory_review::KeptAll>),
    post("/memory/edit", input::<crate::memory_review::EditNoteReq>, "Edit a note (a new version), or write a new one with its type, scope, title and body.").returns(schema::<crate::memory_index::NoteView>),
    post("/memory/promote", input::<crate::memory_review::PromoteReq>, "Promote an agent's scratch note into a shared, kept note in a scope (bakery, baker, project:<id> or workspace:<thread>), edited if wanted; its versions stay.").returns(schema::<crate::memory_index::NoteView>),
    post("/memory/retire", input::<crate::memory_review::RetireReq>, "Retire a note: it leaves the wiki and every search; its history stays.").returns(schema::<crate::memory_review::Retired>),
    get("/memory/digest", none, "The memory's digest: proposals waiting for the operator (Keep all keeps them), notes gone stale, and the last day.").returns(schema::<crate::memory_review::MemoryDigest>),
    get("/memory/timeline", input::<crate::memory_review::TimelineQuery>, "What happened in the memory, the newest first: proposed, kept, edited, written, declined and retired.").returns(schema::<crate::memory_review::MemoryTimeline>),
    get("/backlog", input::<crate::backlog::BacklogQuery>, "The backlog: what is to be done next, for the Bakery or a project, in the operator's order: each item's why, status (queued, doing, done, dropped), who added it, its threads and its history. An agent sees its project's items and the Bakery's.").returns(schema::<crate::backlog::Backlog>),
    post("/backlog/add", input::<crate::backlog::AddReq>, "Add an item to the end of the backlog: something worth doing later, with why. An agent's goes into its project's backlog unless it names the scope (bakery, or a project), marked as its own.").returns(schema::<crate::backlog::BacklogItem>),
    post("/backlog/edit", input::<crate::backlog::EditReq>, "Change a backlog item's title, why, scope or linked threads.").returns(schema::<crate::backlog::BacklogItem>),
    post("/backlog/status", input::<crate::backlog::StatusReq>, "Move a backlog item along: queued, doing, done or dropped.").returns(schema::<crate::backlog::BacklogItem>),
    post("/backlog/move", input::<crate::backlog::MoveReq>, "Put a backlog item just above another, or at the end.").returns(schema::<crate::backlog::Backlog>),
    post("/backlog/remove", input::<crate::backlog::IdReq>, "Remove a backlog item.").returns(schema::<crate::backlog::Backlog>),
    get("/toolkit/skill", input::<crate::toolkit::SkillQuery>, "Read a skill your conversation has, by name: when one fits the work, read it before you start.").returns(schema::<crate::toolkit::Skill>),
    get("/conversation/brief", input::<crate::briefs::BriefQuery>, "A conversation's brief: the standing instructions its agent gets every turn, who set it, whether an agent's still waits for the operator, and every change.").returns(schema::<crate::briefs::Brief>),
    post("/conversation/brief", input::<crate::briefs::SetBriefReq>, "Set another conversation's brief. The operator's holds as theirs; the Baker's, or an orchestrator's in the same project, is a recommendation, in effect until the operator turns it down. An agent never sets its own.").returns(schema::<crate::briefs::Brief>),
    post("/conversation/brief/decide", input::<crate::briefs::DecideBriefReq>, "Keep, or turn down, an agent's brief; turned down, the brief before it holds again.").returns(schema::<crate::briefs::Brief>),
    get("/move-in/sessions", none, "The operator's Claude Code sessions of the Bakery's own folders, newest first, to move into the Bakery: each one's transcript, its size, how often it was compacted, its title (its custom title first), the conversation it moved into, and where it can go on: a new workspace, a workspace's orc, a worker under one in its worktree, the Baker's own conversation or a Baker thread, the best fit suggested.").returns(schema::<crate::move_in::ClaudeSessions>),
    post("/move-in/import", input::<crate::move_in::ImportReq>, "Move a Claude Code session into the Bakery with nothing lost, where the operator picks: one of its destinations (a new workspace's orc, a worker under a workspace in the worktree it has, the Baker's own conversation, a Baker thread, or a conversation already there). A copy of its transcript goes where that conversation's Claude runtime reads sessions, and its next turn resumes it. Again, what the original gained since comes along while the thread has not carried it on. Refused (409), with nothing changed: a conversation carrying on another session or in the middle of a turn, a session already moved in elsewhere, and one both sides went on with. The original is only read.").returns(schema::<crate::move_in::Imported>),
    get("/move-in/history", input::<crate::move_in_history::MoveInHistoryQuery>, "A moved-in conversation's old conversation, read from the transcript copy its move-in made, as it came in: the operator's and Claude's messages, tool calls in short form (what a tool answered is never shown, only counted), compactions marked with their summary. Newest first, a page at a time: pass the last page's `earlier` as `before`. Keys and tokens are left out; what a page reads past without showing is counted in `skipped`, a line that can't be read says so, and a text cut short says how long it was.").returns(schema::<crate::move_in_history::MoveInHistory>),
    get("/fresh-start/plan", input::<crate::fresh_start::PlanQuery>, "What a fresh start would do now: the Baker thread it carries over (the one named, else the one called local models), the conversations it leaves behind in the old Heart, the Always-allow grants it clears, the worktrees left on disk, and why it can't go ahead (a turn running, or no single thread to keep).").returns(schema::<crate::fresh_start::Plan>),
    post("/thread/merge", input::<crate::baker_threads::MergeThreadReq>, "Merge work waiting in the review where it goes: a chef's branch into main, as its spawner's thread_merge does (then the chef's thread is archived and its worktree and branch are removed, anything not committed going to the Trash first); a workspace's branch into its repository's main, or a workspace worker's into its workspace, only on the operator's approval and exactly as the review showed it. As a merge commit, into a clean checkout; a conflict merges nothing.").returns(schema::<crate::baker_threads::ThreadMerged>),
    get("/merge/waiting", none, "Work waiting to merge, newest first: chefs' and workspaces' branches for main, and workspace workers' branches their orc proposed for its workspace. Each with its branch, what it merges into, its commits, the files it changes, and lines added and removed.").returns(schema::<crate::baker_threads::MergeWaitingList>),
    post("/merge/ready", input::<crate::baker_threads::MergeReadyReq>, "Propose work for the merge review: a workspace's orc proposes a worker's branch for its workspace, or its own branch for main. Everything committed; a note says what it is and how it was tested. Asking again replaces it.").returns(schema::<crate::baker_threads::MergeWaiting>),
    post("/workspace", input::<crate::workspaces::CreateWorkspaceReq>, "Open a workspace: an orchestrator's conversation on a worktree of its own, a new branch off its repository's main, which opens workers as its peers. The Bakery's own beneath the Baker, or a catalog project's (project_id) on that project's repository; its work merges into that project's main through the merge review.").returns(schema::<crate::workspaces::Workspace>),
    post("/delete/plan", input::<crate::deletes::DeletePlanReq>, "What deleting a project, one repository of a project, a workspace, or a thread or chat would remove and what stays, changing nothing: its threads, the worktrees and branches the Bakery made (unsaved work in them kept in git first), rooms, containers, volumes, networks, the engines it could have used that are off (started to clear, then stopped), folders the Bakery made going to the Trash, the operator's own folders the trash_own tick takes, project roots let go, folders and branches kept because something staying uses them, why it can't go ahead (the main Baker, a turn running), and its digest.").returns(schema::<crate::deletes::DeletePlan>),
    post("/delete/confirm", input::<crate::deletes::DeleteConfirmReq>, "Carry out exactly the plan the operator saw, while a fresh plan still has its digest (409 when it changed, or is refused): unsaved work kept in git first (nothing goes that couldn't be kept), each thread discarded with its history kept, what the plan removes removed, engines it could have used that were off started and stopped again, folders to the Trash, a project retired. Recorded as delete.done; answers what went and each step that didn't.").returns(schema::<crate::deletes::DeleteDone>),
    get("/trash", none, "The Bakery's Trash: unsaved work deletes kept in git, in every repository the Bakery worked in, newest first: repo, name, when, its commits outside main and its uncommitted changes. Each stays until the operator empties the Trash.").returns(schema::<crate::git_trash::TrashList>),
    post("/trash/put-back", input::<crate::git_trash::TrashRef>, "Put an entry of the Trash back: its branch made again at its tip and a worktree on it, its uncommitted changes restored exactly; it leaves the Trash.").returns(schema::<crate::git_trash::PutBack>),
    post("/trash/empty", input::<crate::git_trash::EmptyReq>, "Empty the Trash of the entries the operator saw and confirmed, one or all of them: the unsaved work in them is gone for good. Entries kept since are not touched; answers how many went and each that couldn't.").returns(schema::<crate::git_trash::Emptied>),
    post("/message/send", input::<crate::messages::SendReq>, "Send a message or a note to another conversation, as SendMessage does between sessions: one in your project, any Baker conversation when you are one, or the Baker (to: baker). A message starts a turn there, after any turn running, so send one only when they must act on it. Anything else goes as a note (note: true): it shows there without starting a turn, and their next turn reads it. Your conversation shows it sent, then taken up or read.").returns(schema::<crate::messages::Sent>),
    get("/saved", input::<crate::routes::saved::SavedQuery>, "The operator's saved replies, newest first: one conversation's, or every conversation's.").returns(schema::<crate::routes::saved::SavedList>),
    post("/saved", input::<crate::routes::saved::SaveReq>, "Save a finished reply by its agent.turn.done event; saving it again answers the one already kept.").returns(schema::<crate::routes::saved::SavedReply>),
    post("/saved/remove", input::<crate::routes::saved::SavedId>, "Unsave a reply.").returns(schema::<crate::routes::saved::Unsaved>),
    post("/saved/tick", input::<crate::routes::saved::TickReq>, "Tick or untick one checklist item of a saved reply.").returns(schema::<crate::routes::saved::SavedReply>),
    post("/memory/import", input::<crate::routes::memory::MemoryImportReq>, "Import a folder of Claude Code memory notes into a conversation's memory (a Baker thread's into the one Baker memory), keeping each note's name, type, description and links.").returns(schema::<crate::routes::memory::MemoryImported>),
    get("/context", input::<StreamQuery>, "The context a stream's next turn starts from.").returns(schema::<crate::routes::memory::ContextView>),
    post("/worktree", input::<OpenWorktreeReq>, "Create a named git worktree for a project (legacy shape of project/open).").returns(schema::<crate::api_catalog::EventShape>),
    post("/env/provision", input::<ProvisionReq>, "Build a stream's sandbox room (its toolchain image).").returns(schema::<crate::routes::env::ProvisionResp>),
    post("/env/exec", input::<AttachReq>, "Run one command (argv, no shell) in a stream's sandbox room and return its output.").returns(schema::<crate::routes::env::ExecResp>),
    post("/env/pty", input::<crate::routes::env::AttachReq>, "Interactive terminal attach.").returns(schema::<crate::routes::env::ExecResp>),
    get("/env", input::<EnvQuery>, "A stream's sandbox room status.").returns(schema::<crate::routes::env::EnvStatusResp>),
    post("/terminal/create", input::<crate::routes::terminal::CreateReq>, "Open an interactive terminal: a thread's (its stream_id, host or room), this Mac's from anywhere (stream_id sys:host: a login shell in the operator's home), or a project's (stream_id project:<id>: in its main folder, on this Mac, in its room or in a container as its sandbox setting says; mode is then ignored).").returns(schema::<crate::terminals::TerminalInfo>),
    get("/terminal/list", input::<ListQuery>, "The studio terminals open on a stream: a thread's, or project:<id> for a project's.").returns(schema::<Vec<crate::terminals::TerminalInfo>>),
    post("/terminal/rename", input::<RenameReq>, "Rename a studio terminal.").returns(schema::<crate::terminals::TerminalInfo>),
    get("/terminal/read", input::<crate::routes::terminal::ReadQuery>, "What one of the thread's terminals printed last, as text, and whether something is running in it.").returns(schema::<crate::routes::terminal::TerminalText>),
    post("/terminal/close", input::<CloseReq>, "Close a studio terminal and kill its process tree.").returns(untyped),
    post("/terminal/ticket", input::<crate::routes::terminal::TicketReq>, "Terminal connection ticket.").returns(schema::<crate::routes::terminal::TicketResp>),
    get("/terminal/ws", none, "Terminal WebSocket."),
    get("/jobs", input::<JobListQuery>, "Jobs started with job_start, running or ended, newest first.").returns(schema::<Vec<crate::jobs::JobView>>),
    post("/jobs/start", input::<JobStartReq>, "Start a long command (a build, tests, a dev server) in a conversation's sandbox and return at once. It shows live in a studio pane, and the conversation that started it (yours, when you call this) is told when it ends.").returns(schema::<crate::jobs::JobView>),
    get("/jobs/output", input::<JobOutputQuery>, "A job's output from an offset (since 0, then the last call's next), with its state and exit code.").returns(schema::<crate::jobs::JobOutput>),
    post("/jobs/stop", input::<JobStopReq>, "Stop a running job.").returns(schema::<crate::jobs::JobView>),
    // ── git: the operator's git client (routes::git) ──
    get("/git/status", input::<crate::routes::git::GitQuery>, "A conversation's checkout: branch, upstream, ahead/behind, and each changed file, staged or not.").returns(schema::<crate::routes::git::GitStatus>),
    get("/git/branches", input::<crate::routes::git::GitQuery>, "Local branches with the current one marked, their upstream and tracking, and last commit.").returns(schema::<Vec<crate::routes::git::Branch>>),
    get("/git/log", input::<crate::routes::git::LogQuery>, "Recent commits of the current branch or a named one.").returns(schema::<Vec<crate::routes::git::Commit>>),
    get("/git/diff", input::<crate::routes::git::DiffQuery>, "The diff of unstaged changes, staged ones, one file, one commit, or a whole branch against a base, what is not committed yet too if asked.").returns(schema::<crate::routes::git::Diff>),
    post("/git/stage", input::<crate::routes::git::StageReq>, "Stage paths, or every change; or take them out of the staged set.").returns(schema::<crate::routes::git::GitStatus>),
    post("/git/commit", input::<crate::routes::git::CommitReq>, "Commit what is staged, with a message.").returns(schema::<crate::routes::git::Commit>),
    post("/git/branch", input::<crate::routes::git::BranchReq>, "Create a branch, from the current commit or a named one, and switch to it.").returns(schema::<crate::routes::git::GitStatus>),
    post("/git/switch", input::<crate::routes::git::SwitchReq>, "Switch the checkout to another branch.").returns(schema::<crate::routes::git::GitStatus>),
    post("/git/fetch", input::<crate::routes::git::FetchReq>, "Fetch a remote's branches (default origin) without touching the checkout; status then shows ahead/behind.").returns(schema::<crate::routes::git::GitStatus>),
    post("/git/pull", input::<crate::routes::git::GitQuery>, "Bring the current branch up to its upstream with a merge, never a rebase; a conflict aborts it and changes nothing.").returns(schema::<crate::routes::git::GitStatus>),
    post("/git/update", input::<crate::routes::git::GitQuery>, "Bring a thread up to the branch it forked from with a merge, never a rebase, fetching it first; a conflict stops it and lists the files to resolve.").returns(schema::<crate::routes::git::Updated>),
    post("/git/rebase", input::<crate::routes::git::RebaseReq>, "Rebase a thread onto a branch, with the reason you rebase instead of merging (git_update) recorded first; an agent's own git rebase is refused without one. A conflict stops it and lists the files, the rebase left in progress.").returns(schema::<crate::routes::git::Updated>),
    post("/git/push", input::<crate::routes::git::PushReq>, "Push a branch to origin and track it. Never forced.").returns(schema::<crate::routes::git::Done>),
    post("/google/change", input::<crate::swap::GoogleChange>, "The operator's say on one change to Google Cloud with their own sign-in (anything but a read). The thread's door asks this itself when its room sends one, holds the request, and sends it once approved; calling it yourself sends nothing.").returns(schema::<String>),
    post("/git/pr", input::<crate::routes::git::PrReq>, "Open a GitHub pull request for the current branch (gh).").returns(schema::<crate::routes::git::Done>),
    post("/git/merge", input::<crate::routes::git::MergeReq>, "Merge a branch into the current one; a conflict aborts it and changes nothing.").returns(schema::<crate::routes::git::Done>),
    get("/git/merge-preview", input::<crate::routes::git::MergePreviewQuery>, "What merging the current branch into base would change on base, its conflicts included, without touching the checkout (git merge-tree): each file with its lines added and removed (the first 200, all counted); with diff, the diff too, cut at 32 KiB.").returns(schema::<crate::routes::git::MergePreview>),
    get("/git/merge-preview/chain", input::<crate::routes::git::MergeChainQuery>, "Merge several branches into base in turn (base with the first, that with the next, and so on) without touching the checkout: each step's conflicts, so branches that conflict only with each other show up, then each file with its lines (the first 200, all counted); with diff, the combined diff too, cut at 32 KiB.").returns(schema::<crate::routes::git::MergeChain>),
    get("/git/pr/checks", input::<crate::routes::git::PrChecksQuery>, "The checks on the current branch's pull request and how each stands (gh).").returns(schema::<crate::routes::git::PrChecks>),
    // ── GitHub, through the operator's own gh login ──
    get("/github/repos", input::<crate::routes::github::ReposQuery>, "The operator's GitHub repositories and their organizations': visibility, default branch, when updated, and where each is cloned on this Mac, if it is.").returns(schema::<crate::routes::github::Repos>),
    post("/github/clone", input::<crate::routes::github::CloneReq>, "Clone a GitHub repository into a new folder in the operator's home (~/Dev/code/<name> by default) and open it as a Project.").returns(schema::<crate::routes::github::Cloned>),
    get("/github/issues", input::<crate::routes::github::IssuesQuery>, "A repository's issues: open by default, most recent first.").returns(schema::<Vec<crate::routes::github::Issue>>),
    get("/github/issue", input::<crate::routes::github::IssueQuery>, "One issue with its body and comments.").returns(schema::<crate::routes::github::IssueView>),
    post("/github/issue/create", input::<crate::routes::github::IssueCreateReq>, "Open an issue on a repository. It is published under the operator's name, so it always asks first.").returns(schema::<crate::routes::github::Published>),
    post("/github/issue/comment", input::<crate::routes::github::IssueCommentReq>, "Comment on an issue. It is published under the operator's name, so it always asks first.").returns(schema::<crate::routes::github::Published>),
    get("/github/scope", none, "The pull request inbox's scope: the operator, their organizations and any other owner added, each on or off, and repositories turned on or off by name.").returns(schema::<crate::routes::github_prs::GithubScope>),
    post("/github/scope", input::<crate::routes::github_prs::ScopeReq>, "The operator turns owners and repositories on or off for the inbox; a login not listed yet is added.").returns(schema::<crate::routes::github_prs::GithubScope>),
    get("/github/prs", none, "The open pull requests across the inbox's scope, most recently updated first: checks, reviews, mergeability, and the thread and project each belongs to.").returns(schema::<crate::routes::github_prs::Prs>),
    post("/github/prs/refresh", none, "Ask GitHub for the inbox's pull requests again now.").returns(schema::<crate::routes::github_prs::Prs>),
    get("/github/pr", input::<crate::routes::github_prs::PrQuery>, "One pull request in full: its text, files, diff, checks, reviews, review threads, comments and commits.").returns(schema::<crate::routes::github_prs::PrView>),
    post("/github/pr/review", input::<crate::routes::github_prs::ReviewReq>, "The operator reviews a pull request: a comment, an approval or a request for changes, with line comments.").returns(schema::<crate::routes::github_prs::Reviewed>),
    post("/github/pr/comment", input::<crate::routes::github_prs::CommentReq>, "The operator comments on a pull request, or replies in one of its review threads.").returns(schema::<crate::routes::github_prs::Commented>),
    post("/github/pr/merge", input::<crate::routes::github_prs::PrMergeReq>, "The operator merges a pull request, or squashes it, deleting its branch if asked.").returns(schema::<crate::routes::github_prs::Merged>),
    post("/github/pr/ready", input::<crate::routes::github_prs::ReadyReq>, "The operator marks a draft pull request ready for review.").returns(schema::<crate::routes::github_prs::Ready>),
    post("/github/pr/thread/resolve", input::<crate::routes::github_prs::ResolveReq>, "The operator resolves a review thread, or opens it again.").returns(schema::<crate::routes::github_prs::Resolved>),
    get("/github/runs", none, "The latest CI run of each workflow on each branch, across the inbox's repositories and the operator's Projects'.").returns(schema::<crate::routes::github_prs::Runs>),
    get("/github/run", input::<crate::routes::github_prs::RunQuery>, "One CI run with its jobs and their steps.").returns(schema::<crate::routes::github_prs::RunView>),
    get("/github/run/log", input::<crate::routes::github_prs::JobLogQuery>, "A CI job's log, step by step; each step keeps its last 64 KiB.").returns(schema::<crate::routes::github_prs::RunLog>),
    post("/github/run/rerun", input::<crate::routes::github_prs::RerunReq>, "The operator re-runs a CI run, or only its failed jobs.").returns(schema::<crate::routes::github_prs::Queued>),
    get("/grants", input::<crate::grants::GrantsQuery>, "The grants a conversation holds: each lets it push one exact branch of one repository without asking first.").returns(schema::<Vec<crate::grants::PushGrant>>),
    post("/grants/give", input::<crate::grants::GiveReq>, "The operator lets a conversation push a branch without asking first: its current branch by default, fixed to that name now.").returns(schema::<crate::grants::PushGrant>),
    get("/remote/waiting", none, "Every remote action waiting for the operator across all conversations (pushes, pull requests, issues and comments), with their count.").returns(schema::<crate::remote::WaitingList>),
    get("/remote/history", input::<crate::remote::HistoryQuery>, "The whole Bakery's remote actions done, newest first: pushes, pull requests, issues, comments, reviews, merges and re-runs, with who did them.").returns(schema::<Vec<crate::remote::RemoteAction>>),
    post("/grants/revoke", input::<crate::grants::EndReq>, "End a grant: the operator takes it back, or the conversation's own agent gives it up.").returns(schema::<Vec<crate::grants::PushGrant>>),
    get("/git/provenance", input::<crate::provenance::ProvenanceQuery>, "For each hunk a branch committed since base (base...HEAD), the recorded write that put it there: conversation, turn and tool call, with how many of its lines that write accounts for; none for a shell command, a hand edit or code from base.").returns(schema::<crate::provenance::Provenance>),
    // ── the app's own state ──
    get("/ui/layout", none, "The app's layout: workspaces, each a strip of columns of panels, plus the sidebar and the Baker dock. Null until one is saved.").returns(schema::<Option<crate::routes::ui::Layout>>),
    post("/ui/layout", input::<crate::routes::ui::SaveLayoutReq>, "Save the app's layout; an open app follows it.").returns(schema::<crate::routes::ui::Layout>),
    get("/ui/appearance", none, "The app's appearance: its themes and the one in use. Null until one is saved.").returns(untyped),
    post("/ui/appearance", input::<crate::routes::ui::SaveAppearanceReq>, "Save the app's appearance; an open app follows it.").returns(untyped),
    get("/ui/bundle", input::<crate::routes::ui_bundle::BundleQuery>, "A conversation's own build of the app's UI (its bakery-client/build, from `pnpm --dir bakery-client build`): the hash the app checks it against, its size, the branch and commit it was built on, whether bakery-client has uncommitted changes, and the UI the app shows now.").returns(schema::<crate::routes::ui_bundle::Bundle>),
    post("/ui/bundle/install", input::<crate::routes::ui_bundle::InstallReq>, "Make a conversation's UI build, named by the hash ui_bundle answered, the app's UI: the open app checks it and reloads, nothing restarts. Always asks the operator, since the app runs it with their full access; refused if the build changed since. ui_bundle_rollback puts the previous UI back.").returns(schema::<crate::routes::ui_bundle::Installed>),
    post("/ui/bundle/rollback", none, "Put back the app's UI from before the last ui_bundle_install, say when a screenshot shows the new one broken; the open app checks it and reloads. One way only: rolling forward again takes a new install and its approval.").returns(schema::<crate::routes::ui_bundle::RolledBack>),
    // ── paired devices ──
    post("/pair", input::<crate::devices::PairReq>, "Pair a phone or a node with the one-time code Bakery shows; answers the device's own token. The tailnet's only route without a token.").returns(schema::<crate::devices::PairResp>),
    get("/devices", none, "The paired devices, and whether this node is reachable over the tailnet (if not, what to turn on).").returns(schema::<crate::devices::Devices>),
    post("/devices/pair-code", none, "Show a one-time pairing code: 8 characters, 5 minutes, one use.").returns(schema::<crate::devices::PairCode>),
    post("/devices/revoke", input::<crate::devices::RevokeReq>, "Unpair a device: its token stops working and its connections close at once.").returns(schema::<crate::devices::Devices>),
    post("/devices/remote", input::<crate::devices::RemoteReq>, "Make this node reachable from the operator's other devices over Tailscale, or not.").returns(schema::<crate::devices::RemoteStatus>),
    get("/outside-window", none, "The outside window's read-only token: when it was made and last used, where it is reached and what it may call.").returns(schema::<crate::outside_window::OutsideWindow>),
    post("/outside-window/create", none, "Make the outside window's read-only token, shown this once. One at a time.").returns(schema::<crate::outside_window::Created>),
    post("/outside-window/revoke", none, "Revoke the outside window's read-only token: it stops working at once.").returns(schema::<crate::outside_window::OutsideWindow>),
    // ── the desktop app's previews ──
    get("/desktop/ws", none, "The desktop app's link: the previews it shows, and the commands it runs on them."),
    post("/preview/snapshot", input::<crate::desktop::PreviewReq>, "What your conversation's open preview shows: its URL, title and visible text.").returns(schema::<crate::desktop::PreviewSnapshot>),
    post("/preview/navigate", input::<crate::desktop::PreviewNavigateReq>, "Load an http(s) URL in your conversation's open preview.").returns(schema::<crate::desktop::PreviewPage>),
    post("/preview/click", input::<crate::desktop::PreviewClickReq>, "Click the first element a CSS selector matches in your conversation's open preview.").returns(schema::<crate::desktop::PreviewPage>),
    post("/desktop/screenshot", input::<crate::desktop::ScreenshotReq>, "A screenshot of the Bakery window as the operator sees it, say to check a UI change: you get the image itself beside the result. The app's own page only; a Code or Preview view is never in it. Each capture is noted in your conversation.").returns(schema::<crate::desktop::Screenshot>),
    post("/preview/eval", input::<crate::desktop::PreviewEvalReq>, "Run a JavaScript expression in your conversation's open preview and get its value as JSON. It runs in that page as the operator left it, logged-in sessions included.").returns(schema::<crate::desktop::PreviewValue>),
    get("/system/ghostty", none, "Whether the Ghostty terminal app is available.").returns(schema::<crate::routes::system::GhosttyAvailable>),
    post("/system/ghostty/open", input::<GhosttyOpenReq>, "Open Ghostty on the operator's screen in a stream's worktree.").returns(schema::<crate::routes::system::GhosttyOpenResp>),
    get("/stream/{id}", none, "One stream's recorded facts: project, branch, worktree, lifetime.").returns(schema::<Vec<crate::api_catalog::EventShape>>),
    post("/seat/assign", input::<SeatAssignReq>, "Assign a model id (a model_options id) to a conversation's seat. While the seat's turn runs, the switch waits for that turn to end (a seat.model.pending answer) and the turn runs on; a second switch replaces it, and one back to what the seat has cancels it.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/pipe/gate/decide", input::<crate::routes::pipe::ChildToolGateDecideReq>, "Decide a native permission request.").returns(schema::<crate::api_catalog::EventShape>),
    post("/stream/lifetime", input::<LifetimeReq>, "Set a stream's lifetime: living or burn.").returns(schema::<crate::routes::lifecycle::LifetimeResp>),
    post("/stream/discard", input::<DiscardReq>, "Throw away a stream's worktree and sandbox room; its history is kept.").returns(schema::<crate::routes::lifecycle::DiscardResp>),
    get("/files/list", input::<FilesQuery>, "One directory level of a stream's worktree.").returns(schema::<Vec<crate::tools::DirEntryInfo>>),
    get("/files/read", input::<FilesQuery>, "A file's full text from a stream's worktree, with its digest.").returns(schema::<crate::routes::project::FileReadResp>),
    post("/files/write", input::<crate::routes::project::FileWriteReq>, "The operator's editor save.").returns(schema::<crate::routes::project::FileWriteResp>),
    get("/events", none, "Event WebSocket."),
    post("/agent/turn", input::<crate::routes::agent::AgentTurnReq>, "Run one agent turn synchronously.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/attachments", input::<crate::attachments::AttachmentUploadReq>, "Upload an attachment from the native picker.").returns(schema::<crate::attachments::AttachmentRef>),
    get("/attachment", input::<crate::attachments::AttachmentQuery>, "An attachment sent on a thread, as it was sent, with its media type.").returns(untyped),
    post("/agent/turn/queue", input::<AgentQueueReq>, "Queue a message for a conversation's agent; it runs when the agent is free.").returns(schema::<crate::api_catalog::QueuedShape>),
    post("/agent/turn/review", input::<crate::snapshots::ReviewTurnReq>, "Review all the changes one agent turn made at once: keep every one, or undo every one.").returns(schema::<crate::snapshots::TurnReviewed>),
    get("/agent/change", input::<crate::snapshots::ChangeQuery>, "One change an agent's turn made, before and after, read from Bakery's own copy rather than the history.").returns(schema::<crate::snapshots::ChangeContent>),
    get("/agent/snapshot-limit", none, "The largest file, in MB, kept a copy of when a turn's changes are recorded: a bigger file's change can only be kept, never undone. 10 MB until the operator sets it.").returns(schema::<crate::snapshots::SnapshotLimit>),
    post("/agent/snapshot-limit", input::<crate::snapshots::SnapshotLimit>, "Set that size limit, in MB (at least 1). A turn already running keeps the limit it began with.").returns(schema::<crate::snapshots::SnapshotLimit>),
    post("/search", input::<crate::search::SearchReq>, "The operator's own search: file names and text in this workspace or everywhere the Bakery has a folder, secret files included with their values masked. Folders git ignores are skipped. Never recorded.").returns(schema::<crate::search::Searched>),
    post("/search/reveal", input::<crate::search::RevealReq>, "One masked line from a secret file, shown whole, for the operator alone. Never recorded.").returns(schema::<crate::search::Revealed>),
    post("/agent/turn/queue/steer-all", input::<AgentTurnCancelReq>, "Turn every queued follow-up into one steer for the agent's running turn.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/turn/queue/withdraw", input::<crate::routes::agent::WithdrawReq>, "Take back a message still waiting in an agent's queue, a follow-up or a steer not yet delivered, to edit or drop it; its text comes back. Refused once it is sent, merged, or on its way into the running turn.").returns(schema::<crate::agent_control::Withdrawn>),
    get("/agent/tool-payload", input::<ToolPayloadQuery>, "The complete payload behind one native tool call record.").returns(untyped),
    post("/agent/turn/steer", input::<AgentQueueReq>, "Steer a conversation's running turn with a message.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/turn/cancel", input::<AgentTurnCancelReq>, "Stop a conversation's running turn and pause its queue (queued messages are kept).").returns(untyped),
    post("/agent/dispatch/resume", input::<AgentTurnCancelReq>, "Resume a paused conversation queue.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/mode", input::<AgentModeReq>, "Set a conversation's operating mode: manual, plan, auto_edit or auto.").returns(schema::<crate::api_catalog::EventShape>),
    get("/agent/tools", input::<crate::api_tools::ToolsQuery>, "What a conversation's agents may use of the Bakery's tools: all, read (only those graded read) or none.").returns(schema::<crate::api_tools::Tools>),
    post("/agent/tools", input::<crate::api_tools::ToolsReq>, "Set what a conversation's agents may use of the Bakery's tools: all, read or none.").returns(schema::<crate::api_tools::Tools>),
    post("/agent/runtime/config", input::<NativeConfigReq>, "Set one native runtime option (model, effort, ...) a session advertises.").returns(schema::<crate::pipe::AcpRuntimeSnapshot>),
    post("/agent/selection", input::<SelectionReq>, "Set the model and effort a conversation on the Bakery's own loop runs every turn on, as its picker shows them.").returns(schema::<crate::selection::Selection>),
    get("/agent/activity", input::<AgentActivityQuery>, "What a conversation's agent is doing right now: phase, running tool, finished tools.").returns(schema::<Option<crate::state::AgentActivity>>),
    post("/agent/accept", input::<crate::routes::agent::AgentAcceptReq>, "Accept a write proposal.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/answer", input::<crate::routes::agent::AgentAnswerReq>, "Answer an agent's question.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/resume", input::<crate::routes::agent::AgentResumeReq>, "Resume a turn that died with the process.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/session/choose", input::<crate::routes::agent::SessionChoiceReq>, "Answer a session that didn't resume: retry it, or start fresh.").returns(schema::<crate::api_catalog::EventShape>),
    post("/agent/reject", input::<crate::routes::agent::AgentDecisionReq>, "Reject a write proposal.").returns(schema::<crate::api_catalog::EventShape>),
    get("/baker", none, "The permanent Baker conversation.").returns(schema::<crate::routes::baker::BakerHome>),
    get("/baker/threads", input::<ListThreadsQuery>, "The Baker's conversations: the main one and every parallel thread.").returns(schema::<Vec<crate::baker_threads::BakerThreadView>>),
    post("/baker/threads", input::<CreateThreadReq>, "Open a new Baker thread; for another agent, a peer of its own (a workspace's orc: a worker).").returns(schema::<crate::baker_threads::CreateThreadResp>),
    post("/baker/threads/rename", input::<RenameThreadReq>, "Rename a Baker thread.").returns(schema::<crate::baker_threads::BakerThreadView>),
    post("/baker/threads/archive", input::<ArchiveThreadReq>, "Archive or restore a Baker thread.").returns(schema::<crate::baker_threads::BakerThreadView>),
    get("/baker/threads/read", input::<ThreadReadQuery>, "A conversation's work: its runtime, model, effort and mode, its latest messages, and its last finished turn's result (status, stop reason, tokens, acceptance).").returns(schema::<crate::baker_threads::ThreadRead>),
    post("/baker/threads/wait", input::<ThreadWaitReq>, "Wait until a conversation has nothing left to run, up to 50 s, then return what thread_read returns, plus done:false when it is still working.").returns(schema::<crate::baker_threads::ThreadWaited>),
    get("/baker/threads/board", input::<crate::thread_board::BoardQuery>, "Every thread you look after, in one call (an orc: itself and its workers; the Baker: everything, its conversations first, then every workspace, worker and project thread in use; another agent: itself and the peers it opened): each one's state and last line, its branch, and from git, against main (a workspace's worker against its workspace's branch too), the commits not in yet, whether they are merged or it made none, and whether the rest merges cleanly.").returns(schema::<crate::thread_board::Board>),
    post("/baker/assign", input::<BakerAssignReq>, "Start a legacy Baker branch job on a repository.").returns(schema::<crate::routes::baker::BakerAssignResp>),
    get("/baker/work", none, "Legacy Baker branch jobs.").returns(schema::<crate::routes::baker::BakerWorkList>),
    get("/baker/work/detail", input::<BakerWorkDetailQuery>, "One legacy Baker branch job.").returns(schema::<crate::routes::baker::BakerWorkDetail>),
    post("/baker/push", input::<BakerBranchReq>, "Push a Baker branch to its remote.").returns(schema::<crate::routes::baker::BakerActionResp>),
    post("/baker/pr", input::<BakerPrReq>, "Open a pull request for a Baker branch.").returns(schema::<crate::routes::baker::BakerActionResp>),
    post("/baker/merge", input::<BakerMergeReq>, "Merge a Baker branch.").returns(schema::<crate::routes::baker::BakerActionResp>),
    get("/baker/capabilities", input::<CapabilitiesQuery>, "What the Baker can do: its operations and their summaries.").returns(schema::<crate::routes::baker::CapabilitiesView>),
    post("/task/create", input::<TaskCreateReq>, "Create a task: its durable identity and completion contract.").returns(schema::<crate::tasks::TaskCreateResp>),
    post("/task/start", input::<TaskStartReq>, "Materialize a task's workspace and start its agent.").returns(schema::<crate::tasks::TaskStartResp>),
    post("/task/message", input::<TaskMessageReq>, "Queue a steering message in a task's inbox.").returns(schema::<crate::tasks::TaskMessageResp>),
    post("/task/cancel", input::<TaskCancelReq>, "Cancel a task.").returns(schema::<crate::tasks::TaskCancelResp>),
    post("/task/retire", input::<TaskRetireReq>, "Retire a task and release its worktree and sandbox room.").returns(schema::<crate::tasks::TaskRetireResp>),
    get("/task/list", input::<TaskListQuery>, "The tasks on one Project: status, contract, warnings.").returns(schema::<Vec<crate::tasks::TaskView>>),
    get("/task/review", input::<TaskReviewQuery>, "A task's review: its diff against the baseline and its checks.").returns(schema::<crate::task_review::TaskReviewResp>),
    post("/task/signoff", input::<crate::task_review::TaskSignoffReq>, "Sign off a task's review.").returns(schema::<crate::task_review::TaskSignoffResp>),
    post("/task/push", input::<TaskShipReq>, "Push a task's branch to its remote.").returns(schema::<crate::task_ship::TaskShipResp>),
    post("/task/pr", input::<TaskShipReq>, "Open a pull request for a task's branch.").returns(schema::<crate::task_ship::TaskShipResp>),
    post("/task/integrate", input::<TaskShipReq>, "Merge a signed-off task into its project's branch.").returns(schema::<crate::task_ship::TaskShipResp>),
    get("/operator/snapshot", none, "One snapshot of the node: streams, gates, agents.").returns(schema::<crate::routes::operator::OperatorSnapshot>),
    get("/auth/status", none, "Provider credential status.").returns(schema::<bakery_agent::secrets::AuthStatus>),
    get("/auth/cli-status", input::<crate::routes::auth::CliStatusQuery>, "CLI login status.").returns(schema::<crate::routes::auth::CliAccountsStatus>),
    get("/auth/claude-native/status", none, "Claude login status.").returns(schema::<crate::claude_native::ClaudeNativeStatus>),
    post("/auth/api-key", input::<crate::routes::auth::ApiKeyReq>, "Set an API key.").returns(schema::<bakery_agent::secrets::AuthStatus>),
    post("/auth/clear", input::<crate::routes::auth::ClearReq>, "Clear a credential.").returns(schema::<bakery_agent::secrets::AuthStatus>),
    post("/auth/model", input::<crate::routes::auth::ModelReq>, "Set a provider model.").returns(schema::<bakery_agent::secrets::AuthStatus>),
    post("/auth/xai/device/start", none, "Start an xAI device login.").returns(schema::<bakery_agent::xai_oauth::DeviceStart>),
    post("/auth/xai/device/poll", input::<crate::routes::auth::DevicePollReq>, "Poll an xAI device login.").returns(schema::<bakery_agent::xai_oauth::DevicePollResult>),
    post("/auth/gemini/import-cli", none, "Import the Gemini CLI login.").returns(schema::<crate::routes::auth::ImportResp>),
    post("/auth/claude-code/login/start", none, "Start a Claude Code login.").returns(schema::<bakery_agent::claude_code_slot::LoginStart>),
    post("/auth/claude-code/login/complete", input::<crate::routes::auth::ClaudeCodeCompleteReq>, "Complete a Claude Code login.").returns(schema::<bakery_agent::claude_code_slot::LoginResult>),
    post("/auth/plug-secret", input::<crate::routes::auth::PlugSecretReq>, "Set a plug secret.").returns(schema::<crate::routes::auth::PlugSecretStatus>),
    post("/auth/plug-secret/clear", input::<crate::routes::auth::PlugSecretClearReq>, "Clear a plug secret.").returns(schema::<crate::routes::auth::PlugSecretStatus>),
    get("/providers", none, "The configured model providers and runtime plugs.").returns(schema::<crate::routes::providers::PlugListResp>),
    post("/providers/acp-plug", input::<crate::routes::providers::AcpPlugCreateReq>, "Add an ACP runtime plug.").returns(schema::<crate::node::AcpPlugConfig>),
    post("/providers/acp-plug/update", input::<crate::routes::providers::AcpPlugUpdateReq>, "Change an ACP runtime plug.").returns(schema::<crate::node::AcpPlugConfig>),
    post("/providers/acp-plug/delete", input::<crate::routes::providers::PlugIdReq>, "Remove an ACP runtime plug.").returns(untyped),
    post("/providers/openai-compat-plug", input::<crate::routes::providers::OpenAiCompatPlugCreateReq>, "Add an OpenAI-compatible plug.").returns(schema::<crate::node::OpenAiCompatPlugConfig>),
    post("/providers/openai-compat-plug/update", input::<crate::routes::providers::OpenAiCompatPlugUpdateReq>, "Change an OpenAI-compatible plug.").returns(schema::<crate::node::OpenAiCompatPlugConfig>),
    post("/providers/openai-compat-plug/delete", input::<crate::routes::providers::PlugIdReq>, "Remove an OpenAI-compatible plug.").returns(untyped),
    get("/studio/state", none, "The studio's whole state: projects, conversations, seats, models.").returns(schema::<crate::routes::studio::StudioState>),
    get("/models/catalog", input::<ModelCatalogQuery>, "One provider's live models with their effort levels and context windows.").returns(schema::<bakery_agent::ModelCatalog>),
    get("/accounts/capabilities", input::<AccountCapabilitiesQuery>, "What one account's runtime can do: live catalog, settings, auth.").returns(schema::<crate::account_adapter::AccountCapabilities>),
    get("/mcp/tools", none, "MCP tool list.").returns(schema::<Vec<crate::mcp::McpToolWire>>),
    post("/mcp/call", input::<crate::mcp::McpCallReq>, "MCP tool call.").returns(schema::<crate::mcp::McpCallResp>),
    post("/mcp", none, "The outside window: read-only MCP over HTTP, for its read-only token.").returns(untyped),
    post("/events/ticket", none, "Event WebSocket ticket.").returns(untyped),
];

/// One route as the Baker sees it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Tool {
    pub(crate) name: String,
    pub(crate) method: Method,
    pub(crate) path: &'static str,
    pub(crate) risk: Risk,
    pub(crate) summary: &'static str,
    /// Declared to the model directly; the rest are found with `api_find`.
    pub(crate) menu: bool,
    pub(crate) input: Value,
    /// How far an agent other than the Baker may reach with it; `None` is
    /// the Baker's only.
    pub(crate) agents: Option<Reach>,
    /// Asks the operator before every call, in every mode, whatever its
    /// grade (`api_tools::gate`).
    pub(crate) always_ask: bool,
}

/// How far an agent that is not the Baker reaches with a tool: what it acts
/// on must be in its own project, or its own conversation when it has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reach {
    /// It belongs to no project: GitHub reads, health, versions.
    Any,
    /// Any conversation (or the project itself) in its own project.
    Project,
    /// Its own conversation only.
    Own,
    /// A Bakery-wide list, shown with its own project's entries only.
    Listed,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Overrides {
    format: String,
    routes: BTreeMap<String, Override>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Override {
    name: Option<String>,
    risk: Option<Risk>,
    /// Why the Baker never sees this route.
    hidden: Option<String>,
    menu: Option<bool>,
    /// The kinds of paired device that may call it over the tailnet
    /// (`devices::gate`); none unless named.
    remote: Option<Vec<String>>,
    /// How far other agents may reach with it (`Reach`); the Baker's only
    /// unless named.
    agents: Option<Reach>,
    /// Asks the operator before every call, in every mode, Plan included;
    /// its card still shows its grade.
    always_ask: Option<bool>,
}

fn overrides() -> Overrides {
    let o: Overrides = serde_json::from_str(include_str!("api_overrides.json"))
        .expect("api_overrides.json is valid");
    assert_eq!(o.format, "bakery/api-overrides@1", "api_overrides.json format");
    o
}

/// Whether a paired device of `kind` ("phone", "node") may call this route
/// over the tailnet: only when its override names that kind.
pub(crate) fn remote_allows(method: Method, path: &str, kind: &str) -> bool {
    static REMOTE: OnceLock<BTreeMap<String, Vec<String>>> = OnceLock::new();
    REMOTE
        .get_or_init(|| {
            overrides()
                .routes
                .into_iter()
                .filter_map(|(key, o)| Some((key, o.remote?)))
                .collect()
        })
        .get(&format!("{} {path}", method.as_str()))
        .is_some_and(|kinds| kinds.iter().any(|k| k == kind))
}

fn key(route: &Route) -> String {
    format!("{} {}", route.method.as_str(), route.path)
}

/// `/task/create` → `task_create`. A path that also serves GET names its
/// POST `…_set`.
fn default_name(route: &Route) -> String {
    let base = route
        .path
        .split('/')
        .filter(|s| !s.is_empty() && !s.starts_with('{'))
        .collect::<Vec<_>>()
        .join("_")
        .replace('-', "_");
    let shared = ROUTES
        .iter()
        .any(|r| r.path == route.path && r.method == Method::Get);
    if route.method == Method::Post && shared {
        format!("{base}_set")
    } else {
        base
    }
}

/// Every route the Baker may call, built once.
pub(crate) fn tools() -> &'static [Tool] {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        let overrides = overrides();
        ROUTES
            .iter()
            .filter_map(|route| {
                let o = overrides.routes.get(&key(route));
                if o.is_some_and(|o| o.hidden.is_some()) {
                    return None;
                }
                let o = o.map(|o| (o.name.clone(), o.risk, o.menu, o.agents, o.always_ask));
                let (name, risk, menu, agents, always_ask) = o.unwrap_or_default();
                Some(Tool {
                    name: name.unwrap_or_else(|| default_name(route)),
                    method: route.method,
                    path: route.path,
                    // An ungraded write asks first, as vendo's guard does;
                    // a test keeps every visible POST graded anyway.
                    risk: risk.unwrap_or(match route.method {
                        Method::Get => Risk::Read,
                        Method::Post => Risk::Destructive,
                    }),
                    summary: route.summary,
                    menu: menu.unwrap_or(false),
                    input: tool_input(route),
                    agents,
                    always_ask: always_ask.unwrap_or(false),
                })
            })
            .collect()
    })
}

/// Every route with the name and grade it carries, for the generated UI
/// client: a hidden route keeps its derived name and has no grade, since no
/// agent ever calls it.
#[cfg(test)]
pub(crate) fn listing() -> Vec<(&'static Route, String, Option<Risk>)> {
    let overrides = overrides();
    ROUTES
        .iter()
        .map(|route| {
            match tools().iter().find(|t| t.method == route.method && t.path == route.path) {
                Some(tool) => (route, tool.name.clone(), Some(tool.risk)),
                None => {
                    let named = overrides.routes.get(&key(route)).and_then(|o| o.name.clone());
                    (route, named.unwrap_or_else(|| default_name(route)), None)
                }
            }
        })
        .collect()
}

pub(crate) fn find(name: &str) -> Option<&'static Tool> {
    tools().iter().find(|t| t.name == name)
}

/// The route's input plus a string for each `{param}` in its path.
fn tool_input(route: &Route) -> Value {
    // Reduced to the subset every provider's function calling accepts
    // (Gemini takes no `$ref` and no type unions).
    let mut inline = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = (route.input)(&mut inline)
        .map(simplify)
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    // Who acted is the caller's scope, never an argument.
    if let Some(props) = schema["properties"].as_object_mut() {
        props.remove("actor");
    }
    for param in route.path.split('/').filter_map(|s| s.strip_prefix('{')?.strip_suffix('}')) {
        schema["properties"][param] = json!({"type": "string"});
        match schema["required"].as_array_mut() {
            Some(required) => required.push(json!(param)),
            None => schema["required"] = json!([param]),
        }
    }
    schema
}

/// Keeps `type` (one), `properties`, `required`, `items`, `enum`, `anyOf`
/// and a one-line `description`; drops the rest, and folds an optional
/// field's `["string", "null"]` and a unit enum's `oneOf` of consts.
fn simplify(schema: Value) -> Value {
    let Value::Object(mut obj) = schema else {
        return schema;
    };
    let mut out = Map::new();
    if let Some(variants) = obj.remove("oneOf").or_else(|| obj.remove("anyOf")) {
        let variants: Vec<Value> = variants
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.get("type") != Some(&json!("null")))
            .cloned()
            .collect();
        let consts: Option<Vec<Value>> = variants
            .iter()
            .map(|v| match (v.get("const"), v.get("enum")) {
                (Some(c), _) => Some(vec![c.clone()]),
                (None, Some(Value::Array(e))) => Some(e.clone()),
                _ => None,
            })
            .collect::<Option<Vec<_>>>()
            .map(|groups| groups.concat());
        match (consts, variants.len()) {
            (Some(values), _) => {
                out.insert("type".into(), json!("string"));
                out.insert("enum".into(), Value::Array(values));
            }
            (None, 1) => {
                if let Value::Object(only) = simplify(variants[0].clone()) {
                    out.extend(only);
                }
            }
            (None, _) => {
                out.insert("anyOf".into(), variants.into_iter().map(simplify).collect());
            }
        }
    }
    match obj.remove("type") {
        Some(Value::Array(types)) => {
            if let Some(t) = types.into_iter().find(|t| t != "null") {
                out.insert("type".into(), t);
            }
        }
        Some(t) => {
            out.insert("type".into(), t);
        }
        None => {}
    }
    if let Some(Value::Object(props)) = obj.remove("properties") {
        let props: Map<String, Value> = props.into_iter().map(|(k, v)| (k, simplify(v))).collect();
        out.insert("properties".into(), Value::Object(props));
    }
    if let Some(items) = obj.remove("items") {
        out.insert("items".into(), simplify(items));
    }
    for keep in ["required", "enum"] {
        if let Some(v) = obj.remove(keep) {
            out.insert(keep.into(), v);
        }
    }
    if let Some(c) = obj.remove("const") {
        out.entry("type").or_insert_with(|| json!("string"));
        out.insert("enum".into(), json!([c]));
    }
    if let Some(Value::String(d)) = obj.remove("description") {
        // The first sentence of the doc comment, on one line.
        let text = d.split_whitespace().collect::<Vec<_>>().join(" ");
        let first = text.find(". ").map_or(text.as_str(), |end| &text[..=end]);
        let first: String = first.chars().take(200).collect();
        if !first.is_empty() {
            out.insert("description".into(), json!(first));
        }
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// What `api_routes()` in main.rs serves, read from its source.
    fn served() -> BTreeSet<String> {
        let src = include_str!("main.rs");
        let body = &src[src.find("fn api_routes()").unwrap()..];
        let body = &body[..body.find("\n}\n").unwrap()];
        let flat: String = body.split_whitespace().collect();
        let mut out = BTreeSet::new();
        for chunk in flat.split(".route(\"").skip(1) {
            let (path, rest) = chunk.split_once('"').unwrap();
            for (method, call) in [("GET", "get("), ("POST", "post(")] {
                if rest.contains(call) {
                    out.insert(format!("{method} {path}"));
                }
            }
        }
        out
    }

    #[test]
    fn the_catalog_lists_exactly_the_routes_the_router_serves() {
        let listed: BTreeSet<String> = ROUTES.iter().map(key).collect();
        assert_eq!(listed.len(), ROUTES.len(), "a route is listed twice");
        let served = served();
        assert!(served.len() > 100, "the router parse found {served:?}");
        let missing: Vec<_> = served.difference(&listed).collect();
        let extra: Vec<_> = listed.difference(&served).collect();
        assert!(missing.is_empty(), "served but not in the catalog: {missing:?}");
        assert!(extra.is_empty(), "in the catalog but not served: {extra:?}");
    }

    #[test]
    fn overrides_name_real_routes_and_every_visible_write_is_graded() {
        let o = overrides();
        let listed: BTreeSet<String> = ROUTES.iter().map(key).collect();
        for k in o.routes.keys() {
            assert!(listed.contains(k), "override for a route that does not exist: {k}");
        }
        for route in ROUTES {
            let o = o.routes.get(&key(route));
            let hidden = o.is_some_and(|o| o.hidden.is_some());
            if route.method == Method::Post && !hidden {
                assert!(
                    o.and_then(|o| o.risk).is_some(),
                    "{} is visible with no risk grade",
                    key(route)
                );
            }
            assert!(
                hidden || !route.path.contains("/auth/"),
                "{} exposes credentials to the Baker",
                key(route)
            );
        }
    }

    #[test]
    fn tool_names_are_unique_and_valid() {
        let mut seen = BTreeSet::new();
        for tool in tools() {
            assert!(
                !tool.name.is_empty()
                    && tool.name.len() <= 64
                    && tool
                        .name
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{:?} is not a valid tool name",
                tool.name
            );
            assert!(seen.insert(tool.name.clone()), "{} is used twice", tool.name);
            assert_eq!(tool.input["type"], "object", "{}", tool.name);
        }
    }

    #[test]
    fn operator_only_routes_are_hidden() {
        let visible = |method: Method, path: &str| {
            tools().iter().any(|t| t.method == method && t.path == path)
        };
        for path in [
            "/agent/accept",
            "/agent/reject",
            "/agent/answer",
            "/agent/resume",
            "/agent/session/choose",
            "/agent/pipe/gate/decide",
            "/machine-access",
            "/machine-access/check",
            "/machine-access/requests/decide",
            "/machine-access/reach",
            "/machine-access/containers",
            "/machine-access/seat-size",
            "/code/open",
            "/code/close",
            "/code/goto",
            "/model-proxy/install",
            "/model-proxy/start",
            "/model-proxy/keys/remove",
            "/machine-access/claude-container/sign-in",
            "/agents/tool-policy",
            "/agents/peer-limit",
            "/agents/always-allowed/remove",
            "/policy/write-mode",
            "/control/bakery",
            "/events/ticket",
            "/terminal/ticket",
            "/agents/storage/delete",
            "/files/write",
            "/task/signoff",
            "/providers/acp-plug",
            "/mcp/call",
            "/mcp",
            "/outside-window/create",
            "/outside-window/revoke",
            "/github/scope",
            "/github/pr/review",
            "/github/pr/comment",
            "/github/pr/merge",
            "/github/pr/ready",
            "/github/pr/thread/resolve",
            "/github/run/rerun",
            "/grants/give",
            "/disk/clean",
            "/engine/colima/idle-stop",
            "/engine/colima/size",
            "/engine/docker-default",
            "/engine/orbstack/install",
            "/delete/plan",
            "/delete/confirm",
            "/trash/put-back",
            "/trash/empty",
        ] {
            assert!(!visible(Method::Post, path), "POST {path} is visible to the Baker");
        }
        for path in ["/agents/storage", "/agents/storage/file", "/auth/status", "/events", "/terminal/ws", "/desktop/ws", "/trash", "/outside-window"] {
            assert!(!visible(Method::Get, path), "GET {path} is visible to the Baker");
        }
    }

    #[test]
    fn schemas_reduce_to_what_every_provider_accepts() {
        let seat = find("seat_assign").expect("seat_assign is a tool");
        let text = seat.input.to_string();
        assert!(
            !text.contains("$ref") && !text.contains("$defs") && !text.contains("$schema"),
            "{text}"
        );
        assert_eq!(seat.input["properties"]["seat"]["type"], "string", "{text}");
        assert!(
            seat.input["properties"]["seat"]["enum"]
                .as_array()
                .is_some_and(|e| e.contains(&json!("baker"))),
            "{text}"
        );
        let stream = tools().iter().find(|t| t.path == "/stream/{id}").unwrap();
        assert_eq!(stream.input["required"], json!(["id"]));
        // An optional field is a plain type, never a ["string","null"] union.
        let memory = tools().iter().find(|t| t.path == "/memory/write").unwrap();
        assert_eq!(memory.input["properties"]["id"]["type"], "string", "{}", memory.input);
        // A tagged enum's variants keep their tag values.
        let project = tools().iter().find(|t| t.path == "/project/create").unwrap();
        assert!(project.input.to_string().contains(r#""enum":["attach"]"#), "{}", project.input);
        // The caller never picks who it acted as.
        let queue = tools().iter().find(|t| t.path == "/agent/turn/queue").unwrap();
        assert!(queue.input["properties"].get("actor").is_none(), "{}", queue.input);
    }
}
