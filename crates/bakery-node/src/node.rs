//! Node identity and the multi-node **stance** (hooks only — see `MULTI_NODE.md`).
//!
//! PHASE: this sprint deliberately implements identity + policy + an ownership
//! *record*. It does NOT implement replication, leases, fencing, or handoff.
//! CONTROL_PLANE §9 keeps ungraceful handoff open, so anything that would create
//! a second writer for a stream is out of scope on purpose.
//!
//! What exists here:
//! - `node.json` in `~/.bakery` — stable `node_id` + `stream_fallback` policy.
//! - A claim record (`stream.ownership.claimed`, appended to the stream itself by
//!   `main.rs`) saying which node holds the write for a stream.
//!
//! The rule being honoured: **exclusive write-ownership per stream at any
//! instant** (CONTROL_PLANE §9). The local node claims the streams it opens;
//! nothing transfers ownership yet, and no remote node is contacted.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a stream should live when the local machine is not the only option.
/// Stored, surfaced, and honoured only as far as `local_only` semantics today —
/// the remote arms of this enum are *declared*, not implemented.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamFallback {
    /// Never consider another node. Today's real behaviour for every value.
    LocalOnly,
    /// Prefer this machine; fall back to a server node when it is unavailable.
    PreferLocal,
    /// Prefer a persistent server node; use local only when it is unreachable.
    PreferServer,
    /// Let the node decide from reachability + battery/lid state.
    Auto,
}

impl Default for StreamFallback {
    fn default() -> Self {
        // Local-first: the machine in front of the human is the execution plane.
        StreamFallback::PreferLocal
    }
}

/// How a proposed `agent.file.changed` reaches **disk** (write gate).
///
/// Per-node today (see `NodeConfig::write_mode`) — same file, same load/save
/// path as `stream_fallback`; a per-stream override is future work.
///
/// **Orthogonal to AGENT_CAPABILITY.md § modes** (Manual / Plan / Auto Edit /
/// Auto Go): those gate *capability breadth* (tools, commands, autonomy of
/// the agent loop). `WriteMode` only gates *when a file proposal hits disk*.
/// Compose: e.g. Auto Go + Manual write = full tools but human accept on
/// files; Manual capability + Auto write is unusual and still evented.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WriteMode {
    /// Every write is gated: propose, human reads the bytes, human accepts.
    /// The only mode with no trust surface — default for anyone who has not
    /// opted into more. Aligns with capability "Manual" for the write axis.
    #[default]
    Manual,
    /// Auto-accept narrowed by path rules (Rules v0: globs in node.json).
    /// No counterpart in AGENT_CAPABILITY four-mode list — pure write-gate.
    Rules,
    /// Every proposal is written immediately through the jail. Propose +
    /// accept events still land — roughly capability "Auto Edit" for files
    /// only; Auto Go (commands, etc.) is separate and still hard-gated.
    Auto,
}

/// Outcome of a matched Rules v0 entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    /// Write immediately, same as Auto for this one proposal.
    Allow,
    /// Ask — behaves exactly like Manual for this one proposal.
    Deny,
}

/// One Rules v0 entry: a path glob and what to do when a proposal's
/// `file_path` matches it. File-edit-only for W2 — no HTTP setter, no UI;
/// hand-edit `~/.bakery/node.json`'s `rules` array.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteRule {
    /// Glob relative to the worktree root. `*` matches within one path
    /// segment; `**` matches zero or more segments. No `?`, `[...]`, or
    /// brace expansion — PANELS.md's examples don't need more than this.
    pub pattern: String,
    pub effect: RuleEffect,
}

/// One explicitly named directory copied into an ACP seat's isolated HOME.
/// The source is a host path selected by the operator; the destination is
/// relative to the seat HOME so a CLI can find it at its normal location
/// without inheriting or bind-mounting the host HOME.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpConfigCopy {
    pub source: String,
    pub home_relative_destination: String,
}

/// One named ACP CLI plug (07d-A3B: "Named ACP plugs" — `acp:grok`,
/// `acp:claude`, `acp:codex`, ...). `id` is the operator-chosen model id a
/// seat is assigned (`SeatAssignment.model_id`) and the exact string Heart
/// events record as `model_id`. `argv[0]` is the binary; the rest is passed
/// after it (e.g. `["claude", "agent", "stdio"]` for a `claude` binary on
/// PATH — a bare name resolves the same way `std::process::Command` would,
/// an absolute path is exact). Distinct from the legacy singular
/// `acp_cli`/`acp_args`/`acp_config` trio, which stays live under the fixed
/// `"acp"` id for back-compat with an already-configured node.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpPlugConfig {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub argv: Vec<String>,
    #[serde(default)]
    pub config: Option<AcpConfigCopy>,
    /// Allowlisted directory env for the boxed child (`CODEX_HOME`, `AGY_BIN`,
    /// `AGY_CONVERSATIONS_DIR`). Not a token dump.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub env: std::collections::HashMap<String, String>,
    /// Which MCP transport this CLI can be handed at `session/new`
    /// (07d-B Phase 2 Stage C item 4) — `"stdio"` or `"none"`. Not merely
    /// "supports some MCP configuration": a plug that cannot actually accept
    /// the loopback MCP shim over stdio must say so, so
    /// `routes::studio::model_ready_for_socket` can honestly refuse it for
    /// the permanent Baker or a task socket rather than silently degrading
    /// (every real CLI in this codebase today speaks stdio, hence the
    /// default — `"none"` exists for a plug that is known not to).
    #[serde(default = "default_mcp_transport")]
    pub mcp_transport: String,
}

/// [`AcpPlugConfig::mcp_transport`]'s default — every plug configured before
/// this field existed, and the legacy singular `acp_cli` row (which has no
/// struct of its own to carry this field), are both treated as `"stdio"`.
pub fn default_mcp_transport() -> String {
    "stdio".to_string()
}

impl AcpPlugConfig {
    /// Honest yes/no for "can this plug be handed the loopback MCP shim at
    /// `session/new`" — the one thing [`Self::mcp_transport`] is allowed to
    /// mean `false`. Case/whitespace-insensitive so a hand-edited
    /// `node.json` (`" None "`, `"NONE"`) still refuses rather than silently
    /// passing through.
    pub(crate) fn supports_mcp_stdio(&self) -> bool {
        !self.mcp_transport.trim().eq_ignore_ascii_case("none")
    }
}

/// One named openai-compat plug (07d-A3B: local/self-hosted OpenAI-shaped
/// endpoint — Ollama, llama.cpp, vLLM, HF TGI, an optional gateway sidecar).
/// `id` is the assignable model id, same role as [`AcpPlugConfig::id`]. The
/// bearer (if any) lives in `secrets.json`'s `openai_compat_secrets[id]`,
/// never here — this struct is not secret-shaped.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAiCompatPlugConfig {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    pub base_url: String,
    pub model: String,
}

/// A capability report produced by a subscription account adapter. ACP itself
/// deliberately does not standardize model enumeration, so Bakery records a
/// report only when the connected adapter can actually supply one; an absent
/// report is represented as unavailable, never a guessed model list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SubscriptionCapabilities {
    #[serde(default)]
    pub models: Vec<SubscriptionModelCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionModelCapability {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub effort_levels: Vec<SubscriptionEffortCapability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionEffortCapability {
    pub id: String,
    pub label: String,
}

/// Persistent identity of this daemon. Written once to `~/.bakery/node.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Stable across restarts; `{hostname}-{hex}`. Used as the ownership subject.
    pub node_id: String,
    pub hostname: String,
    #[serde(default)]
    pub stream_fallback: StreamFallback,
    /// Manual / Rules / Auto write gate for every stream this node owns.
    #[serde(default)]
    pub write_mode: WriteMode,
    /// Optional operator budget for native HTTP model rounds per instruction.
    /// None (the default) continues until completion, approval, cancellation,
    /// or a real error. ACP retains its own harness's execution policy.
    #[serde(default)]
    pub agent_max_steps: Option<std::num::NonZeroUsize>,
    /// Optional operator-wide ceiling on DRIVER-issued turns per task, used
    /// only when a task's own contract declares no `turn_budget`. None (the
    /// default) means no hidden ceiling — a task runs until completion,
    /// review, cancellation, a real error, or the stall guard. Explicit and
    /// configurable, never a silent constant.
    #[serde(default)]
    pub task_turn_budget: Option<std::num::NonZeroU32>,
    /// The largest file, in MB, a turn snapshot keeps a copy of (Settings);
    /// a bigger file's change can only be kept. None: the default, 10 MB.
    #[serde(default)]
    pub snapshot_limit_mb: Option<std::num::NonZeroU32>,
    /// The most open peers agents may open in one project. None means
    /// `baker_threads::DEFAULT_PEER_LIMIT`.
    #[serde(default)]
    pub peer_limit: Option<usize>,
    /// Rules v0 (empty by default — Rules mode behaves as Manual until this
    /// is hand-populated).
    #[serde(default)]
    pub rules: Vec<WriteRule>,
    /// Absolute path to The Bakery's own source checkout (for Baker assign).
    /// Durable in this file. `BAKERY_SOURCE_ROOT` env overrides for one process
    /// and is written back here on first resolve so the next start needs no export.
    #[serde(default)]
    pub bakery_source_root: Option<String>,
    /// Stream id of the permanent Baker socket (07d-B Phase 1). Minted
    /// exactly once by `routes::baker::ensure_permanent_baker` and stable
    /// across restarts; the conversation, seat model, and memory all live
    /// on that stream in Heart — this field is only the durable pointer.
    #[serde(default)]
    pub baker_stream_id: Option<String>,
    /// Child-tool kinds the operator always allows in every project, chosen
    /// in Settings' tool rules (`agents::set_tool_policy`). Empty by default:
    /// nothing beyond the always-safe kinds (`ChildToolKind::is_auto_safe`)
    /// is ever pre-approved. The old `allowed_child_tools` list, which an
    /// approval card's "Always" also filled, is dropped by this rename.
    #[serde(default)]
    pub always_kinds: Vec<crate::pipe::ChildToolKind>,
    /// What approval cards' "Always allow" granted: one kind each, in one
    /// project, the Baker's conversations, or one chat
    /// (`pipe::decide_child_tool_gate`).
    #[serde(default)]
    pub always_grants: Vec<crate::pipe::AlwaysGrant>,
    /// Kinds the operator never allows — refused before any allow list or
    /// automatic operating mode, on every runtime. Disjoint from
    /// `always_kinds`.
    #[serde(default)]
    pub denied_child_tools: Vec<crate::pipe::ChildToolKind>,
    /// Absolute path to the ACP CLI binary (e.g. `claude`, `grok`, `codex`)
    /// this node should use for seat assignments of kind `"acp"`. Also
    /// settable per-process via `BAKERY_ACP_CLI` env (that overrides this
    /// field). When absent, `"acp"` seat assignments return a Setup error
    /// and HTTP-API seats remain available.
    #[serde(default)]
    pub acp_cli: Option<String>,
    /// argv passed to the ACP CLI after the binary, e.g. `["agent", "stdio"]`.
    /// `BAKERY_ACP_ARGS` env (whitespace-split) overrides this for one process.
    #[serde(default)]
    pub acp_args: Vec<String>,
    /// One narrowly scoped CLI config directory and its exact location inside
    /// the seat's private HOME. `BAKERY_ACP_CONFIG_SRC` plus
    /// `BAKERY_ACP_CONFIG_DEST` override this pair for one process.
    #[serde(default)]
    pub acp_config: Option<AcpConfigCopy>,
    /// The former source-only shape is retained only to fail with a migration
    /// message instead of silently spawning without the intended config copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_config_src: Option<String>,
    /// Named ACP CLI plugs, keyed by their own `id` (07d-A3B). Additive to
    /// the legacy singular `acp_cli`/`acp_args`/`acp_config` trio above —
    /// both can be configured at once; a seat assigned bare `"acp"` still
    /// resolves the legacy row, one assigned a plug's own `id` resolves here.
    #[serde(default)]
    pub acp_plugs: Vec<AcpPlugConfig>,
    /// Named openai-compat plugs (07d-A3B), keyed by their own `id`.
    #[serde(default)]
    pub openai_compat_plugs: Vec<OpenAiCompatPlugConfig>,
    /// Capability reports keyed by subscription/ACP plug id. This is a
    /// report boundary, not a static Bakery catalog: a missing key means the
    /// adapter did not expose model/effort/context facts.
    #[serde(default)]
    pub subscription_capabilities: std::collections::HashMap<String, SubscriptionCapabilities>,
    /// The operator bearer token every non-exempt HTTP route requires
    /// (loopback is not a trust boundary: any local process — including a
    /// sandboxed ACP child, whose Seatbelt allows network — can reach this
    /// port). Minted once on first load, persisted here (node.json is 0600
    /// and on `bakery_exec`'s sandbox deny list, so children cannot read
    /// it). `BAKERY_OPERATOR_TOKEN` overrides for one process without being
    /// written back. Never handed to any spawned child.
    #[serde(default)]
    pub operator_token: Option<String>,
    /// What the Baker's native runtime may read, write, or run beyond its
    /// sandboxed workspace — operator-granted in Settings → Machine access.
    #[serde(default)]
    pub machine_access: crate::machine_access::MachineAccess,
    /// The model proxy's own secrets (`model_proxy`): its management key,
    /// which the proxy keeps only as a hash. Never handed to any child.
    #[serde(default)]
    pub model_proxy: crate::model_proxy::ProxySecrets,
}

/// 32 random bytes, hex — same `/dev/urandom` source the terminal ticket
/// mint uses. Long-lived (unlike a ticket), so it gets the full 32.
pub(crate) fn mint_operator_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .expect("/dev/urandom must be readable");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Model ids no plug may ever claim (07d-A3B review #7: "An ACP or
/// OpenAI-compatible plug can collide with grok, claude, gemini, acp, or
/// another configured plug, creating duplicate picker rows and precedence-
/// dependent dispatch"). The closed HTTP set plus the legacy singular ACP
/// row's own recognized ids — `state::resolve_model_id_for_seat` checks
/// these exact strings ahead of any plug lookup, so a plug sharing one
/// would never even be reachable, silently.
const RESERVED_MODEL_IDS: &[&str] = &[
    "grok",
    "xai",
    "claude",
    "claude-code",
    "claude-native",
    "gemini",
    "google",
    "auto",
    "stub",
    "acp",
    "grok-acp",
];

impl NodeConfig {
    /// Refuse a new plug id that is reserved or already claimed by another
    /// configured plug (ACP or openai-compat — one shared namespace, since
    /// both are looked up by the same `model_id` string). Case-insensitive,
    /// matching how every model id is already compared everywhere else
    /// (`eq_ignore_ascii_case` in `resolve_model_id_for_seat`/
    /// `resolve_seat_model`). Call before pushing a new plug, never after —
    /// this does not itself mutate `self`.
    pub(crate) fn validate_new_plug_id(&self, id: &str) -> Result<(), String> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err("plug id must not be empty".to_string());
        }
        if RESERVED_MODEL_IDS
            .iter()
            .any(|r| trimmed.eq_ignore_ascii_case(r))
        {
            return Err(format!(
                "'{trimmed}' is a reserved model id and cannot be used for a plug"
            ));
        }
        let collides = self
            .acp_plugs
            .iter()
            .map(|p| p.id.as_str())
            .chain(self.openai_compat_plugs.iter().map(|p| p.id.as_str()))
            .any(|existing| existing.eq_ignore_ascii_case(trimmed));
        if collides {
            return Err(format!("a plug with id '{trimmed}' is already configured"));
        }
        Ok(())
    }

    fn path(base: &Path) -> PathBuf {
        base.join("node.json")
    }

    /// Load `~/.bakery/node.json`, creating it on first run. A corrupt file is
    /// replaced rather than fataling — identity is a convenience record, and the
    /// Heart (not this file) is the durable truth.
    pub fn load_or_create(base: &Path) -> Self {
        let p = Self::path(base);
        if let Ok(text) = std::fs::read_to_string(&p) {
            if let Ok(mut cfg) = serde_json::from_str::<NodeConfig>(&text) {
                if !cfg.node_id.trim().is_empty() {
                    // Older node.json predates operator auth — mint the
                    // token in place so the auth layer always has one.
                    if cfg
                        .operator_token
                        .as_deref()
                        .map_or(true, |t| t.trim().is_empty())
                    {
                        cfg.operator_token = Some(mint_operator_token());
                        let _ = cfg.save(base);
                    }
                    return cfg;
                }
            }
            eprintln!(
                "bakery-node: {} unreadable — regenerating identity",
                p.display()
            );
        }
        let cfg = NodeConfig {
            node_id: mint_node_id(&hostname()),
            hostname: hostname(),
            stream_fallback: StreamFallback::default(),
            write_mode: WriteMode::default(),
            agent_max_steps: None,
            task_turn_budget: None,
            snapshot_limit_mb: None,
            peer_limit: None,
            rules: Vec::new(),
            bakery_source_root: None,
            baker_stream_id: None,
            always_kinds: Vec::new(),
            always_grants: Vec::new(),
            denied_child_tools: Vec::new(),
            acp_cli: None,
            acp_args: Vec::new(),
            acp_config: None,
            acp_config_src: None,
            acp_plugs: Vec::new(),
            openai_compat_plugs: Vec::new(),
            subscription_capabilities: Default::default(),
            operator_token: Some(mint_operator_token()),
            machine_access: Default::default(),
            model_proxy: Default::default(),
        };
        let _ = cfg.save(base);
        cfg
    }

    pub fn save(&self, base: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let p = Self::path(base);
        std::fs::write(&p, text).map_err(|e| e.to_string())?;
        // node.json carries the operator token — owner-only, and set every
        // save (not just create) so a pre-hardening file gets fixed too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

}

fn hostname() -> String {
    // No extra dependency: the OS command is enough for a display/identity name.
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// `{hostname}-{16 hex}` from clock + pid. Uniqueness only has to hold across
/// the handful of machines a person owns, and the value is persisted on first
/// run, so a monotonic-clock mix is sufficient (no `rand` dependency).
fn mint_node_id(host: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mixed = nanos ^ ((std::process::id() as u128) << 64);
    format!(
        "{}-{:016x}",
        slug(host),
        (mixed as u64) ^ ((mixed >> 64) as u64)
    )
}

static MINT_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The one centralized opaque-id minter for every server-created identity in
/// this crate — socket stream ids, Project ids, root ids (07d-A3: "Socket
/// IDs are also server-minted and independent of human names... one
/// centralized production ID generator"). `{prefix}_{16 hex}` from
/// clock+pid+a process-wide atomic counter: `mint_node_id` above only ever
/// runs once per process lifetime and doesn't need the counter, but this is
/// called in tight succession (a burst of creates, a test loop), so a
/// nanosecond-resolution clock alone is not enough to guarantee two calls
/// never mint the same id.
pub(crate) fn mint_id(prefix: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let counter = MINT_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mixed = nanos ^ ((std::process::id() as u128) << 64) ^ ((counter as u128) << 32);
    format!(
        "{prefix}_{:016x}",
        (mixed as u64) ^ ((mixed >> 64) as u64) ^ counter
    )
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_ascii_lowercase()
}

/// Payload of a `stream.ownership.claimed` event. Minimal on purpose: enough to
/// answer "who may write this stream", not enough to pretend it is a lease.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnershipClaim {
    pub node_id: String,
    pub hostname: String,
    pub stream_fallback: StreamFallback,
    /// Always `"local-open"` today: the node claims what the human opened on it.
    pub reason: String,
    /// Loud reminder in the data itself that this is not a fenced lease.
    pub note: String,
}

pub const OWNERSHIP_FAMILY: &str = "stream.ownership.claimed";

impl OwnershipClaim {
    pub fn local(cfg: &NodeConfig) -> Self {
        Self {
            node_id: cfg.node_id.clone(),
            hostname: cfg.hostname.clone(),
            stream_fallback: cfg.stream_fallback,
            reason: "local-open".to_string(),
            note: "advisory record; async replication + lease/fence handoff deferred \
                   (CONTROL_PLANE §9). Single writer assumed."
                .to_string(),
        }
    }
}

/// A stream's declared lifetime (STEERING.md § Sockets). Purely a Heart fact
/// — nothing in this crate ever assigns `Living` or `Burn` on its own; both
/// require an explicit `POST /stream/lifetime` call. A stream with no
/// `stream.lifetime.set` event defaults to `Slice`, matching STEERING's
/// "slice — default for 'new session on this bug'".
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lifetime {
    /// Pinned — "as long as I live". Discard refuses without `confirm: true`.
    Living,
    /// One bug, one PR, one experiment — many of these, thrown away often.
    /// The default: discard removes the worktree/room without asking twice.
    #[default]
    Slice,
    /// Hostile/untrusted. Discard acts the same as `Slice` today (a real
    /// isolation tier — CONTROL_PLANE's "Burn" room/VM — is future work);
    /// what a `Burn` lifetime buys *today* is that the studio must not offer
    /// a host-PTY terminal on it (STEERING: "Hostile / untrusted: do not
    /// offer host").
    Burn,
}

/// Payload of a `stream.lifetime.set` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LifetimeClaim {
    pub lifetime: Lifetime,
}

pub const LIFETIME_FAMILY: &str = "stream.lifetime.set";

/// Payload of a `stream.discarded` event — what discard actually did on
/// disk, kept for lineage. Heart rows are never deleted (04-LIFECYCLE.md);
/// this event is the only record that a stream's worktree/room are gone.
///
/// Worktree removal and room destruction are attempted independently and
/// both outcomes are always recorded here, even when one (or both) failed —
/// a discard that half-succeeds must never vanish without a trace just
/// because an early `?` gave up after the first step. A record with either
/// error present is not a clean discard: `discarded_of` (state.rs) treats it
/// as retriable, not done.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscardRecord {
    pub worktree_removed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_error: Option<String>,
    pub room_destroyed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_error: Option<String>,
    /// A folder the discard left in place, and why: one that is, or holds,
    /// a live project's root. Not an error: the discard is done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept: Option<String>,
    /// The chat's own folder went to the Trash.
    #[serde(default)]
    pub chat_trashed: bool,
    /// Why the chat's folder couldn't go to the Trash; it stayed, and the
    /// discard can be retried.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_error: Option<String>,
}

pub const DISCARD_FAMILY: &str = "stream.discarded";

/// A role on a stream (STEERING.md § Models: "seats ≠ models" — the seat is
/// who/what a turn is answering as; the model id in [`SeatAssignment`] is
/// which client answers it, and the two vary independently). At least these
/// three exist per STEERING's seats+A primitive; more may be added later the
/// same way these were — a closed enum, not an open string, so a typo'd seat
/// name fails to compile rather than silently minting a seat nobody assigns.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Seat {
    /// Talks to the project on the human's behalf — the default seat for an
    /// ordinary stream's turns.
    Orchestrator,
    /// A laborer role (STEERING: "laborer seat = sandboxed ACP child") — not
    /// yet auto-selected by anything in this pass, but assignable so a model
    /// can be pinned to it ahead of the work that will route turns there.
    Worker,
    /// The factory's own seat — a Baker stream's turns default here instead
    /// of `Orchestrator` (STEERING § Baker: "Baker is a seat on the factory
    /// socket with the same agent loop").
    Baker,
}

impl Seat {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Seat::Orchestrator => "orchestrator",
            Seat::Worker => "worker",
            Seat::Baker => "baker",
        }
    }
}

/// `{stream_id}:{seat}` — the stable identity a configured agent keeps
/// across model reassignment, pane close, client reload, and node restart
/// (07d-A-AGENT-IDENTITY: "agent durable, model a plug"). One function so
/// every projection and every Heart event payload that needs to name "this
/// configured agent on this socket" agrees on the exact same string —
/// `routes::studio::AgentInfo.agent_id` and every turn event's `agent_id`
/// field are both built from this, never a second inline `format!`.
pub(crate) fn agent_id(stream_id: &str, seat: Seat) -> String {
    format!("{stream_id}:{}", seat.as_str())
}

/// Payload of a `seat.model.assigned` event — one stream's declaration of
/// which model id answers turns on one of its seats. Heart events accumulate
/// per seat, not one event per stream: `state::resolve_seat` reads every
/// `SEAT_ASSIGNED_FAMILY` event on the stream and keeps the latest one whose
/// `seat` matches, the same "declared fact, most recent wins" shape as
/// [`LifetimeClaim`] but keyed within the family instead of one-per-family.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeatAssignment {
    pub seat: Seat,
    pub model_id: String,
    /// Why, when the switch was decided rather than set (the Baker's
    /// recorded switch); the event that caused it is the causality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The model the seat had, recorded with a decided switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Claude native's own setting: which upstream this seat's requests are
    /// swapped toward (`crate::claude_providers::Provider::id`) —
    /// "subscription" | "console" | "openrouter". `None` on every record
    /// from before this choice existed, and on any other model: read back
    /// as `Provider::Subscription`, today's sign-in, unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

pub const SEAT_ASSIGNED_FAMILY: &str = "seat.model.assigned";

/// Payload of a `seat.model.pending` event: a switch of a seat's model or
/// provider asked for while its turn ran, which waits for that turn to end
/// (`state::TurnLease`) rather than cutting it off. Until then the seat's
/// `seat.model.assigned` stays as it was, so the running process keeps its
/// sign-in at the door (`crate::swap`). A newer one for the seat replaces
/// it, one back to what the seat has cancels it, and any newer assignment
/// of the seat ends it. The event that caused it is its causality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeatSwitch {
    pub seat: Seat,
    pub model_id: String,
    /// Claude native's own setting, as `SeatAssignment::provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Why, for a switch decided rather than set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The seat's `seat.model.assigned` it replaces, by hash; none when the
    /// seat had none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub over: Option<String>,
}

pub const SEAT_SWITCH_FAMILY: &str = "seat.model.pending";

/// Payload of a `seat.model.switch_failed` event: the waiting switch its
/// causality names couldn't land, and why. It waits no more; the seat stays
/// where it was until another switch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeatSwitchFailed {
    pub seat: Seat,
    pub error: String,
}

pub const SEAT_SWITCH_FAILED_FAMILY: &str = "seat.model.switch_failed";

/// A stream's durable factory-vs-project authority (07d-A1 correction: this
/// used to be *inferred* from the stream's name/branch/stream_id matching a
/// `baker-`/`baker/` convention — any ordinary project a human happened to
/// name `baker-whatever` silently inherited the Baker boundary. Recorded
/// once, server-side, by whichever route actually creates the stream —
/// never client-suppliable, never re-derived from a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketKind {
    Project,
    Factory,
    /// A task socket (07d-B Phase 3): project work, not factory plumbing —
    /// `is_factory` checks treat this the same as `Project` (orchestrator/
    /// worker seat rules apply, worker is the task's seat), and the Studio
    /// projection buckets it under its parent Project like any other
    /// project-kind socket. Distinguished from `Project` only so
    /// `SocketInfo.state` can report `"task"` and the client can render it
    /// as a task row rather than an ordinary conversation.
    Task,
}

/// Payload of a `socket.kind.recorded` event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketKindRecord {
    pub kind: SocketKind,
}

pub const SOCKET_KIND_FAMILY: &str = "socket.kind.recorded";

// ─── Project catalog (07d-A3) ───────────────────────────────────────────────
//
// A Project is a first-class Heart record, not a git repo: durable identity,
// zero or more filesystem roots, zero or more member sockets, optional Git
// capability on any root that happens to be one. Its own facts live on a
// dedicated catalog stream keyed by its own minted `project_id` (never a
// name, path, or branch — those may collide and never define identity).
// Membership — which Project, and which of its roots, a socket belongs to —
// is a separate, additive fact recorded on the SOCKET's own stream
// (`SocketProjectSet`), not on the Project's stream, so a socket's home can
// change without rewriting the Project's own history.

/// Payload of `project.created` — the one immutable fact of a Project's
/// birth. `name` here is never overwritten; `project.renamed` records later
/// choices, but a malformed/unreadable rename falls back to this original
/// name (not to an older rename) precisely because it is structurally
/// privileged: set once, verified once, never itself subject to "newest
/// wins" ambiguity.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectStacks {
    /// Toolchain version manager: `none` | `mise` | `proto`.
    #[serde(default = "stack_none")]
    pub toolchain: String,
    /// Task runner: `none` | `moon`.
    #[serde(default = "stack_none")]
    pub tasks: String,
    /// Where its services run, the setup's "Services run in": `apple`
    /// ("Apple container", on its project's own network), `orbstack`
    /// ("OrbStack") or `colima` ("Colima"). Its Docker stacks run on that
    /// engine when it's one, else on the Bakery's default (`engine`). An
    /// old `none` reads as `apple`.
    #[serde(default = "stack_apple", deserialize_with = "services_place")]
    #[schemars(with = "String")]
    pub containers: String,
    /// Secrets: `bakery` | `infisical`.
    #[serde(default = "stack_bakery")]
    pub secrets: String,
    /// Where its agents run, the setup's "Agents run in": `mac` ("Mac
    /// sandbox", `mac_sandbox`) or `container` ("Apple container", a Linux
    /// seat on its project's own network). An old `none` reads as `mac`.
    #[serde(default = "stack_mac", deserialize_with = "agents_place")]
    #[schemars(with = "String")]
    pub sandbox: String,
    /// Its own services (`services`): `redis`. None at first.
    #[serde(default)]
    pub services: Vec<String>,
    /// The sign-ins its rooms may use through their door (`swap`): each
    /// gets a stand-in, never the credential. None at first.
    #[serde(default)]
    pub sign_ins: Vec<String>,
}

fn stack_none() -> String {
    "none".into()
}
fn stack_apple() -> String {
    "apple".into()
}
fn stack_mac() -> String {
    "mac".into()
}

/// "none" is gone from the setup's two places (the operator's answer): one
/// still in a project's record, or sent by an older app, reads as the
/// default.
fn services_place<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    Ok(if value == "none" { stack_apple() } else { value })
}
fn agents_place<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let value = String::deserialize(d)?;
    Ok(if value == "none" { stack_mac() } else { value })
}
fn stack_bakery() -> String {
    "bakery".into()
}

impl Default for ProjectStacks {
    fn default() -> Self {
        Self {
            toolchain: stack_none(),
            tasks: stack_none(),
            containers: stack_apple(),
            secrets: stack_bakery(),
            sandbox: stack_mac(),
            services: Vec::new(),
            sign_ins: Vec::new(),
        }
    }
}

impl ProjectStacks {
    pub fn validate(&self) -> Result<(), String> {
        fn one(field: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
            if allowed.contains(&value) {
                Ok(())
            } else {
                Err(format!(
                    "unknown {field} stack {value:?} — allowed: {}",
                    allowed.join(", ")
                ))
            }
        }
        one("toolchain", &self.toolchain, &["none", "mise", "proto"])?;
        one("tasks", &self.tasks, &["none", "moon"])?;
        one("containers", &self.containers, &["apple", "orbstack", "colima"])?;
        one("secrets", &self.secrets, &["bakery", "infisical"])?;
        one("sandbox", &self.sandbox, &["mac", "container"])?;
        for service in &self.services {
            one("service", service, crate::services::KINDS)?;
        }
        for sign_in in &self.sign_ins {
            one("sign-in", sign_in, &crate::swap::names())?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCreated {
    pub name: String,
    #[serde(default)]
    pub stacks: ProjectStacks,
}
pub const PROJECT_CREATED_FAMILY: &str = "project.created";

/// Payload of `project.renamed`. Latest-wins over the Project's own stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRenamed {
    pub name: String,
}
pub const PROJECT_RENAMED_FAMILY: &str = "project.renamed";

/// Payload of `project.pin.set`. Latest-wins; no event yet means unpinned.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ProjectPinSet {
    pub pinned: bool,
}
pub const PROJECT_PIN_FAMILY: &str = "project.pin.set";

/// Payload: a [`ProjectStacks`], the operator's later choice for the
/// Bakery's own setup of a project (its control plane). Latest wins; none
/// yet means the setup it was created with.
pub const PROJECT_STACKS_FAMILY: &str = "project.stacks.set";

/// Payload of `project.root.attached` — additive, paired with
/// `project.root.detached` by `root_id`; the *current* root set is the fold
/// of every attach/detach on the Project's stream in order, not a
/// latest-wins fact (a Project may have more than one root attached at
/// once, schema allows it even though 07d-A3 only ships a single-root UI).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootAttached {
    pub root_id: String,
    /// Canonicalized (symlinks resolved) at attach time — canonical path
    /// equality is how root de-duplication across Projects is checked.
    pub path: String,
    pub is_git: bool,
}
pub const ROOT_ATTACHED_FAMILY: &str = "project.root.attached";

/// Payload of `project.root.detached`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootDetached {
    pub root_id: String,
}
pub const ROOT_DETACHED_FAMILY: &str = "project.root.detached";

/// Marker family — presence of any event in this family on a Project's
/// stream means retired. No payload fields: retiring is one-way, like
/// `stream.discarded`, so there is nothing to declare beyond "it happened."
pub const PROJECT_RETIRED_FAMILY: &str = "project.retired";

/// Payload of `socket.project.set` — recorded on the SOCKET's own stream
/// (not the Project's), latest-wins, fail-closed to Ungrouped
/// (`project_id: None`) on a missing or malformed newest record, same
/// posture as [`SocketKindRecord`]. `root_id` is `None` for a chat-only
/// socket even when `project_id` is set (the Project may still have zero
/// roots, or the socket may not be scoped to one specific root).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketProjectSet {
    pub project_id: Option<String>,
    pub root_id: Option<String>,
}
pub const SOCKET_PROJECT_FAMILY: &str = "socket.project.set";

/// Payload of `socket.task.set` — recorded on the TASK SOCKET's own stream
/// (07d-B Phase 3 item 2), `socket.project.set`-shaped: latest-wins, one
/// more durable pointer fact alongside (never instead of) the ordinary
/// `socket.project.set` a task socket also carries. `work_order` names the
/// Baker work-order source that proposed this task (for A6: the spec path
/// plus the plan event hash) — free-form, never parsed, only displayed and
/// used for the `task.create` idempotency key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocketTaskSet {
    pub task_id: String,
    pub work_order: String,
}
pub const SOCKET_TASK_FAMILY: &str = "socket.task.set";

/// Payload of `socket.workspace.set` — recorded on a socket's own stream
/// when its workspace is materialized *after* identity already exists
/// (07d-B Phase 3 item 5: "create identity first, workspace at
/// readiness"). Latest-wins, like [`SocketProjectSet`]; when present it
/// overrides the `worktree_path`/`repo_root`/`branch` a socket's own
/// `project.onboarded` event recorded at mint time (`state::context_for`
/// honors the newest one). A second `project.onboarded` append is
/// deliberately NOT the idiom for this — onboarding is a socket's one
/// immutable birth fact; a workspace bound later is a separate, additive
/// one, the same "declare it as its own fact" shape `socket.project.set`
/// already uses for membership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSet {
    pub worktree_path: String,
    pub branch: String,
    pub repo_root: String,
    /// Whether THIS append's writer actually ran `git worktree add` for
    /// `worktree_path` — never inferred from the path's location the way a
    /// stale comment on `state::context_for` once assumed. Mirrors
    /// `routes::project::open_stream`'s own `created_by_bakery`: `true` only
    /// on the branch that actually created the worktree, `false` when an
    /// already-existing directory was adopted instead (a crash-recovery
    /// re-run, or a human's own pre-existing checkout) — `discard_stream`'s
    /// "only remove what we can prove we created" gate depends on this being
    /// honest per-append, not a hard-coded constant.
    pub created_by_bakery: bool,
    /// Why it forked the local main, not the remote's just fetched (rule 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_note: Option<String>,
}
pub const SOCKET_WORKSPACE_FAMILY: &str = "socket.workspace.set";

/// Heart stream-id prefix for a Project's own catalog stream — the minted
/// `project_id` itself (from [`mint_id`]) already carries this prefix, so a
/// Project's catalog stream id and its `project_id` are the exact same
/// string; nothing wraps or re-derives it.
pub const PROJECT_STREAM_PREFIX: &str = "project_";

#[cfg(test)]
mod tests {
    use super::*;

    /// The setup's two places, in the operator's words: agents run in the
    /// Mac sandbox or Apple container, services in Apple container, OrbStack
    /// or Colima. "none" is gone: an old record's, or an older app's, reads
    /// as the default, and nothing new may say it.
    #[test]
    fn the_setups_two_places_read_none_as_their_default() {
        let old: ProjectStacks = serde_json::from_str(r#"{"sandbox":"none","containers":"none"}"#).unwrap();
        assert_eq!((old.sandbox.as_str(), old.containers.as_str()), ("mac", "apple"));
        assert!(old.validate().is_ok());
        let fresh = ProjectStacks::default();
        assert_eq!((fresh.sandbox.as_str(), fresh.containers.as_str()), ("mac", "apple"));
        let picked: ProjectStacks = serde_json::from_str(r#"{"sandbox":"container","containers":"orbstack"}"#).unwrap();
        assert_eq!((picked.sandbox.as_str(), picked.containers.as_str()), ("container", "orbstack"));
        for wrong in [
            ProjectStacks { sandbox: "none".into(), ..Default::default() },
            ProjectStacks { containers: "none".into(), ..Default::default() },
        ] {
            assert!(wrong.validate().is_err(), "{wrong:?}");
        }
    }

    #[test]
    fn creates_and_reloads_identity() {
        let dir = tempfile::tempdir().unwrap();
        let a = NodeConfig::load_or_create(dir.path());
        assert!(!a.node_id.is_empty());
        assert!(dir.path().join("node.json").exists());

        let b = NodeConfig::load_or_create(dir.path());
        assert_eq!(
            a.node_id, b.node_id,
            "node_id must be stable across restarts"
        );
    }

    #[test]
    fn corrupt_identity_file_is_regenerated_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("node.json"), "{ not json").unwrap();
        let cfg = NodeConfig::load_or_create(dir.path());
        assert!(!cfg.node_id.is_empty());
    }

    #[test]
    fn fallback_policy_defaults_to_prefer_local_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = NodeConfig::load_or_create(dir.path());
        assert_eq!(cfg.stream_fallback, StreamFallback::PreferLocal);

        cfg.stream_fallback = StreamFallback::PreferServer;
        cfg.save(dir.path()).unwrap();
        let back = NodeConfig::load_or_create(dir.path());
        assert_eq!(back.stream_fallback, StreamFallback::PreferServer);
        assert_eq!(back.node_id, cfg.node_id);
    }

    #[test]
    fn policy_serializes_snake_case() {
        let j = serde_json::to_string(&StreamFallback::LocalOnly).unwrap();
        assert_eq!(j, "\"local_only\"");
    }

    #[test]
    fn write_mode_defaults_to_manual_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = NodeConfig::load_or_create(dir.path());
        assert_eq!(cfg.write_mode, WriteMode::Manual);

        cfg.write_mode = WriteMode::Auto;
        cfg.save(dir.path()).unwrap();
        let back = NodeConfig::load_or_create(dir.path());
        assert_eq!(back.write_mode, WriteMode::Auto);
        assert_eq!(back.node_id, cfg.node_id);
    }

    #[test]
    fn write_mode_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&WriteMode::Manual).unwrap(),
            "\"manual\""
        );
        assert_eq!(
            serde_json::to_string(&WriteMode::Rules).unwrap(),
            "\"rules\""
        );
        assert_eq!(serde_json::to_string(&WriteMode::Auto).unwrap(), "\"auto\"");
    }

    #[test]
    fn rules_default_to_empty_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = NodeConfig::load_or_create(dir.path());
        assert!(cfg.rules.is_empty());

        let mut cfg = cfg;
        cfg.rules.push(WriteRule {
            pattern: "tests/**".into(),
            effect: RuleEffect::Allow,
        });
        cfg.save(dir.path()).unwrap();
        let back = NodeConfig::load_or_create(dir.path());
        assert_eq!(back.rules.len(), 1);
        assert_eq!(back.rules[0].pattern, "tests/**");
        assert_eq!(back.rules[0].effect, RuleEffect::Allow);
    }

    #[test]
    fn acp_config_copy_roundtrips_as_a_named_source_and_home_destination() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = NodeConfig::load_or_create(dir.path());
        cfg.acp_config = Some(AcpConfigCopy {
            source: "/planted/config-source".into(),
            home_relative_destination: ".config/plug".into(),
        });
        cfg.save(dir.path()).unwrap();

        let back = NodeConfig::load_or_create(dir.path());
        assert_eq!(
            back.acp_config,
            Some(AcpConfigCopy {
                source: "/planted/config-source".into(),
                home_relative_destination: ".config/plug".into(),
            })
        );
    }

    #[test]
    fn node_id_has_no_shell_hostile_characters() {
        let id = mint_node_id("Sam's MacBook Pro.local");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "got {id}"
        );
    }

    #[test]
    fn seat_serializes_snake_case_and_matches_as_str() {
        for (seat, name) in [
            (Seat::Orchestrator, "orchestrator"),
            (Seat::Worker, "worker"),
            (Seat::Baker, "baker"),
        ] {
            assert_eq!(serde_json::to_string(&seat).unwrap(), format!("\"{name}\""));
            assert_eq!(seat.as_str(), name);
        }
    }

    #[test]
    fn mint_id_is_prefixed_and_unique_across_a_tight_burst() {
        let ids: Vec<String> = (0..500).map(|_| mint_id("socket")).collect();
        for id in &ids {
            assert!(id.starts_with("socket_"), "got {id}");
        }
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "every minted id in the burst must be distinct"
        );
    }

    #[test]
    fn mint_id_carries_no_shell_hostile_characters() {
        let id = mint_id("project");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
            "got {id}"
        );
    }

    #[test]
    fn seat_assignment_roundtrips_through_json() {
        let a = SeatAssignment {
            seat: Seat::Baker,
            model_id: "grok".into(),
            reason: None,
            from: None,
            provider: None,
        };
        let json = serde_json::to_string(&a).unwrap();
        assert!(json.contains("\"baker\""));
        let back: SeatAssignment = serde_json::from_str(&json).unwrap();
        assert_eq!(back.seat, Seat::Baker);
        assert_eq!(back.model_id, "grok");
    }
}
