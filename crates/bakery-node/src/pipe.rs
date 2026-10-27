use crate::node::Seat;
use crate::state::AppState;
use agent_client_protocol::schema::v1 as acp;
use agent_client_protocol::{
    Agent, Client, ConnectionTo, JsonRpcResponse, RequestCancellation, Responder, UntypedMessage,
};
use bakery_agent::{
    ChatMsg, EditRequest, Model, NativeConfigSelection, NativeConfigSnapshot, NativeConfigValue,
    ProposedEdit,
};
use futures::AsyncRead;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::os::fd::FromRawFd;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub(crate) const ACP_MODEL_ID: &str = "acp";

/// A message that is a command to the runtime itself (`/compact`,
/// `/context`, `/mcp`, a skill, a plugin's `/name:command`): it reaches the
/// runtime exactly as typed, with nothing in front of it. A message that
/// opens with a path (`/Users/…`) is not one.
pub(crate) fn is_command(text: &str) -> bool {
    let first = text.split_whitespace().next().unwrap_or("");
    first.len() > 1 && first.starts_with('/') && !first[1..].contains('/')
}

/// The longest one ACP line (one JSON-RPC message) may be: far past any
/// real message, a big file or tool output included, so it never cuts a
/// session off; only a runaway line with no end meets it.
const MAX_ACP_FRAME: usize = 256 * 1024 * 1024;
const NATIVE_REVIEW_META_KEY: &str = "bakery.dev/review";
const NATIVE_REVIEW_VERSION: u64 = 1;

// ─── Bounded reader ──────────────────────────────────────────────────────────

struct FrameLimitedAsyncRead<R> {
    inner: R,
    remaining: usize,
    /// What `remaining` starts again at, each new line.
    cap: usize,
}

impl<R: AsyncRead + Unpin> AsyncRead for FrameLimitedAsyncRead<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        // ACP stdio is newline-delimited JSON. Read at most one byte beyond
        // the remaining payload budget so a newline exactly at the boundary
        // can close the frame, while any other byte fails the transport.
        let limit = buf.len().min(self.remaining.saturating_add(1).max(1));
        match Pin::new(&mut self.inner).poll_read(cx, &mut buf[..limit]) {
            Poll::Ready(Ok(n)) if n > 0 => {
                for byte in &buf[..n] {
                    if *byte == b'\n' {
                        self.remaining = self.cap;
                    } else if self.remaining == 0 {
                        return Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ACP frame cap exceeded",
                        )));
                    } else {
                        self.remaining -= 1;
                    }
                }
                Poll::Ready(Ok(n))
            }
            other => other,
        }
    }
}

// ─── Kill-on-drop guard ──────────────────────────────────────────────────────

struct KillGuard(Option<Box<dyn FnOnce()>>);

impl KillGuard {
    fn arm(f: impl FnOnce() + 'static) -> Self {
        KillGuard(Some(Box::new(f)))
    }
    fn disarm(mut self) -> Box<dyn FnOnce()> {
        self.0.take().expect("KillGuard already disarmed")
    }
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

// ─── Persistent session types ────────────────────────────────────────────────

static ACP_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_acp_generation() -> u64 {
    ACP_GENERATION.fetch_add(1, Ordering::Relaxed)
}

/// Credential-free projection of one native ACP session.  This is the only
/// shape allowed across the node/client boundary: adapter argv/env, terminal
/// login commands, and auth metadata that may contain account identifiers are
/// intentionally absent.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpRuntimeSnapshot {
    pub(crate) account_id: String,
    pub(crate) generation: u64,
    pub(crate) runtime: Option<AcpRuntimeIdentity>,
    pub(crate) auth_methods: Vec<AcpAuthMethod>,
    pub(crate) auth_status: Option<AcpAuthStatus>,
    pub(crate) config_options: Vec<AcpConfigOption>,
    pub(crate) modes: Vec<AcpMode>,
    pub(crate) current_mode: Option<String>,
    pub(crate) commands: Vec<AcpCommand>,
    pub(crate) prompt: AcpPromptCapabilities,
    pub(crate) steering: bool,
    /// The adapter advertised Bakery's narrow reviewed-input extension for
    /// native `Edit`/`Write`. This is not an ACP-wide claim: absent means
    /// Bakery only exposes the adapter's ordinary allow/reject controls.
    pub(crate) native_review: bool,
    pub(crate) requested_model: Option<String>,
    pub(crate) effective_model: Option<String>,
    pub(crate) usage: Option<AcpUsage>,
    pub(crate) provenance: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpRuntimeIdentity {
    pub(crate) name: String,
    pub(crate) title: Option<String>,
    pub(crate) version: String,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpAuthMethod {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    /// `agent`, `terminal`, or `environment`.  The latter is reported as a
    /// capability but Bakery never accepts its secret value from React.
    pub(crate) kind: &'static str,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpAuthStatus {
    pub(crate) kind: String,
    pub(crate) label: Option<String>,
    pub(crate) detail: Option<String>,
    pub(crate) plan: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpConfigOption {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) kind: &'static str,
    pub(crate) current_value: serde_json::Value,
    pub(crate) values: Vec<AcpConfigValueOption>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpConfigValueOption {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) group: Option<String>,
    pub(crate) group_name: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpMode {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

/// The native session mode that carries a Bakery operating mode on this
/// runtime, if it advertises one. Bakery's modes are enforced in the
/// permission callback, so the runtime must stay in a mode where it still asks
/// before it edits or runs anything — that is also what lets Review record
/// every native edit. Auto Edit and Auto are Bakery policies, not bypasses
/// inside the runtime: Claude's `acceptEdits`/`bypassPermissions` or Codex's
/// `auto` would skip the callback. Plan also takes a runtime's own read-only
/// mode.
///
/// The ids are the runtimes' own: Claude, Grok Build and Hermes `default`;
/// Cursor `agent`; Codex `read-only` (it asks before every edit and escalated
/// command). Never `ask`: on Cursor and Grok Build that is read-only Q&A,
/// where nothing can be edited even after Allow.
pub(crate) fn native_mode_for<'a>(mode: &str, available: &'a [AcpMode]) -> Option<&'a AcpMode> {
    let candidates: &[&str] = match mode {
        "manual" | "auto_edit" | "auto" => &["default", "agent", "manual", "read-only"],
        "plan" => &["plan", "read-only"],
        _ => &[],
    };
    candidates.iter().find_map(|candidate| {
        available
            .iter()
            .find(|offered| offered.id.eq_ignore_ascii_case(candidate))
    })
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpCommand {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) input_hint: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Default, Serialize)]
pub(crate) struct AcpPromptCapabilities {
    pub(crate) image: bool,
    pub(crate) audio: bool,
    pub(crate) embedded_context: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct AcpUsage {
    pub(crate) used: u64,
    pub(crate) size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpConfigValue {
    Select(String),
    Boolean(bool),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpSteerOutcome {
    Injected,
    PromptRequired { reason: Option<String> },
}

struct AcpSessionShared {
    snapshot: AcpRuntimeSnapshot,
    raw_config_options: Vec<acp::SessionConfigOption>,
    /// Bakery's acknowledged composer mode for this native session. `None`
    /// preserves legacy direct ACP calls that predate the composer control;
    /// `Some` drives the permission callback as an operator-control policy.
    operator_mode: Option<crate::agent_control::OperatingMode>,
    /// The runtime's latest cumulative session cost (`usage_update.cost`);
    /// a turn's own cost is how far it moved across that turn's prompt.
    cost: Option<acp::Cost>,
    /// Settings whose change timed out. The runtime may still apply such a
    /// change late and tell only the request Bakery stopped waiting for, so
    /// until Bakery's own set of that option is acknowledged, its shown value
    /// is sent again before each prompt (`reconfirm_settings`).
    unconfirmed: std::collections::BTreeSet<String>,
    /// Where this session's room reads the stream's attachments
    /// (`attachments::room_dir`), once it is up.
    attachments_dir: Option<PathBuf>,
    /// What one prompt sends as itself, by its provider
    /// (`attachments::inline_budget`).
    inline_budget: usize,
    /// The seat's own temp folder, as the runtime may name it: its writes
    /// there are no workspace change for Review.
    seat_tmp: Vec<PathBuf>,
    /// The adapter tells Claude why Review refused a write.
    review_reasons: bool,
}

impl AcpSessionShared {
    fn new(account_id: String, generation: u64) -> Self {
        Self {
            snapshot: AcpRuntimeSnapshot {
                account_id,
                generation,
                runtime: None,
                auth_methods: Vec::new(),
                auth_status: None,
                config_options: Vec::new(),
                modes: Vec::new(),
                current_mode: None,
                commands: Vec::new(),
                prompt: AcpPromptCapabilities::default(),
                steering: false,
                native_review: false,
                requested_model: None,
                effective_model: None,
                usage: None,
                provenance: "acp_session",
            },
            raw_config_options: Vec::new(),
            operator_mode: None,
            cost: None,
            unconfirmed: Default::default(),
            attachments_dir: None,
            inline_budget: 0,
            seat_tmp: Vec::new(),
            review_reasons: false,
        }
    }

    fn replace_config_options(&mut self, options: Vec<acp::SessionConfigOption>) {
        self.snapshot.config_options = options.iter().map(project_config_option).collect();
        self.snapshot.effective_model = self
            .snapshot
            .config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("model"))
            .and_then(|option| option.current_value.as_str().map(str::to_string));
        self.raw_config_options = options;
    }
}

fn project_config_category(category: &acp::SessionConfigOptionCategory) -> String {
    match category {
        acp::SessionConfigOptionCategory::Mode => "mode".to_string(),
        acp::SessionConfigOptionCategory::Model => "model".to_string(),
        acp::SessionConfigOptionCategory::ThoughtLevel => "thought_level".to_string(),
        acp::SessionConfigOptionCategory::Other(value) => value.clone(),
        _ => "unknown".to_string(),
    }
}

fn project_select_value(
    option: &acp::SessionConfigSelectOption,
    group: Option<(&str, &str)>,
) -> AcpConfigValueOption {
    AcpConfigValueOption {
        id: option.value.0.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
        group: group.map(|(id, _)| id.to_string()),
        group_name: group.map(|(_, name)| name.to_string()),
    }
}

fn project_config_option(option: &acp::SessionConfigOption) -> AcpConfigOption {
    let (kind, current_value, values) = match &option.kind {
        acp::SessionConfigKind::Select(select) => {
            let values = match &select.options {
                acp::SessionConfigSelectOptions::Ungrouped(options) => options
                    .iter()
                    .map(|value| project_select_value(value, None))
                    .collect(),
                acp::SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|group| {
                        group.options.iter().map(|value| {
                            project_select_value(
                                value,
                                Some((group.group.0.as_ref(), group.name.as_str())),
                            )
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            };
            (
                "select",
                serde_json::Value::String(select.current_value.0.to_string()),
                values,
            )
        }
        acp::SessionConfigKind::Boolean(value) => (
            "boolean",
            serde_json::Value::Bool(value.current_value),
            Vec::new(),
        ),
        _ => ("unsupported", serde_json::Value::Null, Vec::new()),
    };
    AcpConfigOption {
        id: option.id.0.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
        category: option.category.as_ref().map(project_config_category),
        kind,
        current_value,
        values,
    }
}

/// The ids of the methods the adapter typed `env_var`. Schema v1 dropped that
/// type, so the typed response parses such a method as agent-managed; only
/// the raw initialize response still says it wants a client credential.
fn env_var_auth_methods(raw_init: &serde_json::Value) -> Vec<String> {
    raw_init["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|method| method["type"] == "env_var")
        .filter_map(|method| method["id"].as_str().map(str::to_string))
        .collect()
}

/// `agent`, `terminal`, or `environment`. An `env_var` method stays
/// `environment` — never one Bakery would hand to `authenticate`.
fn auth_method_kind(method: &acp::AuthMethod, env_var_methods: &[String]) -> &'static str {
    match method {
        acp::AuthMethod::Agent(_)
            if env_var_methods
                .iter()
                .any(|id| id.as_str() == method.id().0.as_ref()) =>
        {
            "environment"
        }
        acp::AuthMethod::Agent(_) => "agent",
        acp::AuthMethod::Terminal(_) => "terminal",
        _ => "unknown",
    }
}

fn project_auth_method(method: &acp::AuthMethod, env_var_methods: &[String]) -> AcpAuthMethod {
    AcpAuthMethod {
        id: method.id().0.to_string(),
        name: method.name().to_string(),
        description: method.description().map(str::to_string),
        kind: auth_method_kind(method, env_var_methods),
    }
}

fn steering_supported(init: &acp::InitializeResponse) -> bool {
    let advertised = |meta: Option<&acp::Meta>| {
        meta.and_then(|meta| meta.get("steering"))
            .and_then(|steering| steering.get("supported"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    // Released adapters advertise extensions in the initialize response's
    // top-level `_meta`. Keep the nested lookup for older adapters that put
    // the marker in `agentCapabilities._meta`.
    advertised(init.meta.as_ref()) || advertised(init.agent_capabilities.meta.as_ref())
}

fn native_review_supported(init: &acp::InitializeResponse) -> bool {
    init.meta
        .as_ref()
        .and_then(|meta| meta.get(NATIVE_REVIEW_META_KEY))
        .is_some_and(|value| {
            value.get("version").and_then(serde_json::Value::as_u64) == Some(NATIVE_REVIEW_VERSION)
                && value
                    .get("tools")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|tools| {
                        tools.iter().any(|tool| tool.as_str() == Some("Edit"))
                            && tools.iter().any(|tool| tool.as_str() == Some("Write"))
                    })
        })
}

enum AcpControlMsg {
    Cancel {
        generation: u64,
        reply_tx: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    SetConfig {
        generation: u64,
        config_id: String,
        value: AcpConfigValue,
        reply_tx: tokio::sync::oneshot::Sender<Result<AcpRuntimeSnapshot, String>>,
    },
    SetMode {
        generation: u64,
        mode_id: String,
        reply_tx: tokio::sync::oneshot::Sender<Result<AcpRuntimeSnapshot, String>>,
    },
    Steer {
        generation: u64,
        text: String,
        attachments: Vec<bakery_agent::NativeAttachment>,
        reply_tx: tokio::sync::oneshot::Sender<Result<AcpSteerOutcome, String>>,
    },
}

pub(crate) enum AcpSlot {
    Spawning {
        generation: u64,
        closed: Arc<AtomicBool>,
    },
    Live(LiveAcpSession),
    /// Old session is being killed and reaped; no new spawn until it's gone.
    Closing {
        generation: u64,
    },
}

impl AcpSlot {
    pub(crate) fn generation(&self) -> u64 {
        match self {
            Self::Spawning { generation, .. } | Self::Closing { generation } => *generation,
            Self::Live(live) => live.generation,
        }
    }
}

#[derive(Clone)]
pub(crate) struct LiveAcpSession {
    prompt_lock: Arc<Mutex<()>>,
    prompt_tx: tokio::sync::mpsc::Sender<AcpPromptMsg>,
    control_tx: tokio::sync::mpsc::Sender<AcpControlMsg>,
    shared: Arc<Mutex<AcpSessionShared>>,
    closed: Arc<AtomicBool>,
    pub(crate) generation: u64,
    pub(crate) pid: u32,
}

impl LiveAcpSession {
    /// Mark this session (and all its clones) as closed so in-flight chat()
    /// calls fail immediately instead of reaching a dead child.
    pub(crate) fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn snapshot(&self) -> AcpRuntimeSnapshot {
        self.shared.lock().unwrap().snapshot.clone()
    }
}

/// Read only the currently-published generation.  A callback owned by an
/// evicted child may still be scheduled briefly after cancellation/reassign;
/// all observable updates check this predicate before touching Heart or the
/// live activity projection.
fn acp_generation_is_current(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    generation: u64,
) -> bool {
    state
        .acp_sessions
        .lock()
        .unwrap()
        .get(&(stream_id.to_string(), seat))
        .is_some_and(|slot| slot.generation() == generation)
}

pub(crate) fn acp_runtime_snapshot(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
) -> Option<AcpRuntimeSnapshot> {
    Some(live_acp_session(state, stream_id, seat)?.snapshot())
}

/// The seat's session while it is live, never starting one.
fn live_acp_session(state: &AppState, stream_id: &str, seat: Seat) -> Option<LiveAcpSession> {
    let sessions = state.acp_sessions.lock().unwrap();
    match sessions.get(&(stream_id.to_string(), seat)) {
        Some(AcpSlot::Live(live)) if !live.is_closed() => Some(live.clone()),
        _ => None,
    }
}

fn current_live_generation(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    generation: u64,
) -> Result<LiveAcpSession, String> {
    let sessions = state.acp_sessions.lock().unwrap();
    match sessions.get(&(stream_id.to_string(), seat)) {
        Some(AcpSlot::Live(live)) if live.generation == generation && !live.is_closed() => {
            Ok(live.clone())
        }
        Some(slot) => Err(format!(
            "stale ACP session generation {generation}; current generation is {}",
            slot.generation()
        )),
        None => Err("the ACP session is no longer live".to_string()),
    }
}

pub(crate) fn set_acp_session_config(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    generation: u64,
    config_id: String,
    value: AcpConfigValue,
) -> Result<AcpRuntimeSnapshot, String> {
    let live = current_live_generation(state, stream_id, seat, generation)?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    live.control_tx
        .blocking_send(AcpControlMsg::SetConfig {
            generation,
            config_id,
            value,
            reply_tx,
        })
        .map_err(|_| "ACP session closed before configuration was sent".to_string())?;
    let snapshot = reply_rx
        .blocking_recv()
        .map_err(|_| "ACP session closed before acknowledging configuration".to_string())??;
    current_live_generation(state, stream_id, seat, generation)?;
    Ok(snapshot)
}

pub(crate) fn set_acp_session_mode(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    generation: u64,
    mode_id: String,
) -> Result<AcpRuntimeSnapshot, String> {
    let live = current_live_generation(state, stream_id, seat, generation)?;
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    live.control_tx
        .blocking_send(AcpControlMsg::SetMode {
            generation,
            mode_id,
            reply_tx,
        })
        .map_err(|_| "ACP session closed before its mode was sent".to_string())?;
    let snapshot = reply_rx
        .blocking_recv()
        .map_err(|_| "ACP session closed before acknowledging its mode".to_string())??;
    current_live_generation(state, stream_id, seat, generation)?;
    Ok(snapshot)
}

fn steer_acp_session(
    live: &LiveAcpSession,
    text: String,
    attachments: Vec<bakery_agent::NativeAttachment>,
) -> Result<AcpSteerOutcome, String> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    live.control_tx
        .blocking_send(AcpControlMsg::Steer {
            generation: live.generation,
            text,
            attachments,
            reply_tx,
        })
        .map_err(|_| "ACP session closed before steering was sent".to_string())?;
    reply_rx
        .blocking_recv()
        .map_err(|_| "ACP session closed before acknowledging steering".to_string())?
}

fn cancel_acp_session(live: &LiveAcpSession) -> Result<(), String> {
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    live.control_tx
        .blocking_send(AcpControlMsg::Cancel {
            generation: live.generation,
            reply_tx,
        })
        .map_err(|_| "ACP session closed before cancellation was sent".to_string())?;
    reply_rx
        .blocking_recv()
        .map_err(|_| "ACP session closed before acknowledging cancellation".to_string())?
}

fn remove_acp_slot_generation(state: &AppState, key: &(String, Seat), generation: u64) {
    let mut sessions = state.acp_sessions.lock().unwrap();
    if sessions
        .get(key)
        .is_some_and(|slot| slot.generation() == generation)
    {
        sessions.remove(key);
    }
}

pub(crate) struct AcpPromptMsg {
    text: String,
    attachments: Vec<bakery_agent::NativeAttachment>,
    /// 07d-A5 review #5: the turn this prompt belongs to, so `session_main`
    /// can hand it to `GateClient` — the only way a native gate/tool-call
    /// callback arriving on the session's own background task (outside
    /// `chat()`'s call stack) can correlate its Heart events, or its
    /// `set_agent_phase` write, with the CURRENT turn rather than a stale
    /// one left over from an abandoned prior prompt.
    turn_id: String,
    delta_tx: std::sync::mpsc::Sender<String>,
    reply_tx: tokio::sync::oneshot::Sender<Result<AcpPromptReply, String>>,
}

struct AcpPromptReply {
    text: String,
    cancelled: bool,
    usage: Option<NativeTurnUsage>,
}

/// One native turn's usage as the runtime reported it — the two counts
/// every transport reports, and the rest.
type NativeTurnUsage = (bakery_agent::TurnUsage, bakery_agent::TurnUsageDetail);

/// What a native turn used, from the runtime's own figures: the prompt
/// response's token counts, the latest context reading, and the turn's
/// cost — how far the cumulative session cost (`usage_update.cost`) moved
/// across the prompt. Sessions always start with `session/new`, so no
/// reading before the prompt means nothing spent yet. No new reading, a
/// changed currency, or a total that went down (a `/clear` resets it)
/// leaves the cost absent rather than guessed. `None` when the runtime
/// reported neither tokens nor a cost.
fn native_turn_usage(
    usage: Option<&acp::Usage>,
    cost_before: Option<&acp::Cost>,
    cost_after: Option<&acp::Cost>,
    context: Option<&AcpUsage>,
) -> Option<NativeTurnUsage> {
    let cost = cost_after
        .filter(|after| Some(*after) != cost_before)
        .and_then(|after| {
            let spent_before = match cost_before {
                Some(before) if before.currency != after.currency => return None,
                Some(before) => before.amount,
                None => 0.0,
            };
            let amount = after.amount - spent_before;
            (amount.is_finite() && amount >= 0.0).then(|| bakery_agent::TurnCost {
                amount,
                currency: after.currency.clone(),
            })
        });
    // Every count zero (a `/compact` turn's, say) is no count at all: the
    // runtime reported nothing, which must not read as "0 in, 0 out".
    let usage = usage.filter(|u| {
        [u.total_tokens, u.input_tokens, u.output_tokens]
            .into_iter()
            .chain([u.thought_tokens, u.cached_read_tokens, u.cached_write_tokens].into_iter().flatten())
            .any(|n| n > 0)
    });
    if usage.is_none() && cost.is_none() {
        return None;
    }
    let tokens = bakery_agent::TurnUsage {
        tokens_in: usage.map(|u| u.input_tokens),
        tokens_out: usage.map(|u| u.output_tokens),
    };
    let context = context.filter(|c| c.size > 0);
    let detail = bakery_agent::TurnUsageDetail {
        thought_tokens: usage.and_then(|u| u.thought_tokens),
        cache_read_tokens: usage.and_then(|u| u.cached_read_tokens),
        cache_write_tokens: usage.and_then(|u| u.cached_write_tokens),
        cost,
        context_used: context.map(|c| c.used),
        context_window: context.map(|c| c.size),
    };
    Some((tokens, detail))
}

// ─── Declared child-tool gate ────────────────────────────────────────────────

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildToolKind {
    Read,
    Search,
    Think,
    Fetch,
    Edit,
    Delete,
    Move,
    Execute,
    SwitchMode,
    Other,
}

impl ChildToolKind {
    pub(crate) fn is_auto_safe(self) -> bool {
        matches!(
            self,
            ChildToolKind::Read
                | ChildToolKind::Search
                | ChildToolKind::Think
                | ChildToolKind::Fetch
        )
    }

    /// Kinds that mutate the workspace directly. On a **task socket** they
    /// are approvable like `Execute`: the task's baseline Review diff +
    /// sign-off pinning are the review surface, and the Seatbelt boundary
    /// is the containment — a native ACP worker that cannot edit at all
    /// cannot do its task. On any other socket they stay denied (the only
    /// review surface there is Manual propose/accept, which native writes
    /// bypass) — but denied *decidedly*, never as a gate no human could
    /// ever grant.
    fn is_workspace_mutation(self) -> bool {
        matches!(
            self,
            ChildToolKind::Edit | ChildToolKind::Delete | ChildToolKind::Move
        )
    }
}

/// Where an approval card's "Always allow" holds.
#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GrantScope {
    /// Every conversation in one project.
    Project,
    /// Every one of the Baker's conversations, and no other agent's.
    Baker,
    /// One chat that is in no project.
    Chat,
}

/// One "Always allow": a kind of action, allowed without asking in one scope.
#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AlwaysGrant {
    pub(crate) scope: GrantScope,
    /// The project (scope project) or the conversation (scope chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    pub(crate) kind: ChildToolKind,
}

/// A grant as the operator sees it, with its project's name.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct GrantView {
    #[serde(flatten)]
    pub(crate) grant: AlwaysGrant,
    /// The project's name (scope project).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
}

impl AlwaysGrant {
    /// The grant an "Always allow" of `kind` makes in `stream_id`: the
    /// Baker's scope, else its project, else that chat alone.
    pub(crate) fn for_stream(state: &AppState, stream_id: &str, kind: ChildToolKind) -> Self {
        let (scope, id) = if crate::baker_ops::is_baker_scope_stream(state, stream_id) {
            (GrantScope::Baker, None)
        } else {
            match crate::state::socket_project(&state.heart.lock().unwrap(), stream_id).0 {
                Some(project_id) => (GrantScope::Project, Some(project_id)),
                None => (GrantScope::Chat, Some(stream_id.to_string())),
            }
        };
        AlwaysGrant { scope, id, kind }
    }

    pub(crate) fn view(self, state: &AppState) -> GrantView {
        let name = match (self.scope, &self.id) {
            (GrantScope::Project, Some(id)) => crate::catalog::project_by_id(state, id).map(|p| p.name),
            _ => None,
        };
        GrantView { grant: self, name }
    }
}

/// The operator's node-wide "never" for a native tool kind — checked before
/// every allow list and automatic mode, for every runtime.
pub(crate) fn tool_policy_denies(state: &AppState, kind: ChildToolKind) -> bool {
    state
        .node
        .lock()
        .unwrap()
        .denied_child_tools
        .contains(&kind)
}

/// Whether a native permission request's title names one of the Baker's
/// Bakery tools — `mcp__bakery__<name>` is how the Claude adapter titles a
/// call to Bakery's own MCP server (the server is registered as `bakery` at
/// `session/new`). Such a call is graded and gated where it runs
/// (`api_tools::call`), the same for every runtime, so it is never asked
/// about twice.
fn is_bakery_tool_call(title: &str) -> bool {
    title
        .strip_prefix("mcp__bakery__")
        .is_some_and(crate::api_tools::is_tool)
}

/// Whether `stream_id` is a task socket — the scope where native
/// workspace-mutating child tools are approvable (see
/// [`ChildToolKind::is_workspace_mutation`]).
fn stream_is_task_socket(state: &AppState, stream_id: &str) -> bool {
    let heart = state.heart.lock().unwrap();
    crate::state::socket_task_id(&heart, stream_id).is_some()
}

pub(crate) struct ChildToolRequest {
    /// The assigned model id that owns this call — a named ACP plug's own
    /// id (e.g. `"acp:codex"`), not a generic `"acp"` literal, so a pending
    /// gate and its Heart record say which configured plug actually asked
    /// (07d-A3B review: "Named plugs are also flattened to plug: 'acp'").
    pub(crate) plug: String,
    pub(crate) kind: ChildToolKind,
    pub(crate) title: String,
    /// The exact file changes the runtime says this call will make (ACP
    /// `ToolCallContent::Diff`). With them, a workspace edit is decidable —
    /// the operator approves what they can see — on any socket.
    pub(crate) diffs: Vec<serde_json::Value>,
}

/// The file changes a permission request itself carries, for review.
fn request_diffs(request: &acp::RequestPermissionRequest) -> Vec<serde_json::Value> {
    request
        .tool_call
        .fields
        .content
        .iter()
        .flatten()
        .filter_map(|content| match content {
            acp::ToolCallContent::Diff(diff) => Some(serde_json::json!({
                "path": diff.path.display().to_string(),
                "old_text": diff.old_text,
                "new_text": diff.new_text,
            })),
            _ => None,
        })
        .collect()
}

pub(crate) const CHILD_TOOL_PENDING_FAMILY: &str = "agent.child_tool.pending";
pub(crate) const CHILD_TOOL_DECIDED_FAMILY: &str = "agent.child_tool.decided";

#[cfg(test)]
pub(crate) enum ChildToolDecision {
    Allow,
    Deny,
}

pub(crate) enum ChildToolAdmission {
    Allow,
    Deny,
    Pending(String),
}

#[cfg(test)]
pub(crate) fn admit_child_tool_call(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    req: &ChildToolRequest,
) -> ChildToolDecision {
    match admit_child_tool_call_detailed(state, stream_id, seat, turn_id, req, false) {
        ChildToolAdmission::Allow => ChildToolDecision::Allow,
        ChildToolAdmission::Deny | ChildToolAdmission::Pending(_) => ChildToolDecision::Deny,
    }
}

pub(crate) fn admit_child_tool_call_detailed(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    req: &ChildToolRequest,
    native_reviewable: bool,
) -> ChildToolAdmission {
    let policy_denied = tool_policy_denies(state, req.kind);
    if req.kind.is_auto_safe() && !policy_denied {
        return ChildToolAdmission::Allow;
    }
    // Workspace mutations are only approvable where a complete review
    // surface exists (task sockets — the baseline diff at sign-off); the
    // node-global allow lists must therefore never be consulted for them
    // elsewhere, or one AllowAlways on a task would silently open every
    // plain socket too.
    let approvable = !policy_denied
        && (native_reviewable
            || !req.kind.is_workspace_mutation()
            || stream_is_task_socket(state, stream_id));
    // What an "Always allow" here grants: looked for now, offered on the card.
    let always = AlwaysGrant::for_stream(state, stream_id, req.kind);
    if approvable {
        let once_key = (stream_id.to_string(), req.kind);
        if state
            .child_tool_allow_once
            .lock()
            .unwrap()
            .remove(&once_key)
        {
            return ChildToolAdmission::Allow;
        }
        let node = state.node.lock().unwrap();
        if node.always_kinds.contains(&req.kind) || node.always_grants.contains(&always) {
            return ChildToolAdmission::Allow;
        }
    }
    // 07d-A1 §2/correction: agent-scoped like every other per-turn family —
    // this seat's own pending gate must never surface in another agent's
    // Chat or prompt (`event_belongs_to_agent` fails closed without it).
    let agent_id = crate::node::agent_id(stream_id, seat);
    let payload = serde_json::json!({
        "plug": req.plug,
        "kind": req.kind,
        "title": req.title,
        "seat": seat,
        "agent_id": agent_id,
        "turn_id": turn_id,
        "native_reviewable": native_reviewable,
        "diffs": req.diffs,
        "always": always.view(state),
    })
    .to_string();
    // A learning scratch thread asks no one: what would wait for the
    // operator is refused, with no card.
    if crate::learning::scratch(state, stream_id) {
        let what = format!("{} ({})", req.title, format!("{:?}", req.kind).to_lowercase());
        crate::learning::refuse(state, stream_id, &what);
        return ChildToolAdmission::Deny;
    }
    if !approvable {
        // Not a gate — no human could ever grant it here. Record the
        // request and the node's own deny durably, and never flip the
        // activity phase to AwaitingApproval: surfacing an undecidable
        // gate teaches the operator that approvals are decorative.
        if let Ok(ev) = crate::state::append(
            state,
            stream_id,
            "node",
            CHILD_TOOL_PENDING_FAMILY,
            payload,
            vec![],
        ) {
            let reason = if policy_denied {
                "your tool policy never allows this kind of action (Settings → Agents & abilities)"
            } else {
                "native workspace mutations are not reviewable on this socket — only task sockets have a baseline review surface"
            };
            let decided = serde_json::json!({
                "decided_event": ev.event_hash,
                "decision": ChildToolGateDecisionKind::Deny,
                "turn_id": turn_id,
                "reason": reason,
            })
            .to_string();
            let _ = crate::state::append(
                state,
                stream_id,
                "node",
                CHILD_TOOL_DECIDED_FAMILY,
                decided,
                vec![ev.event_hash],
            );
        }
        return ChildToolAdmission::Deny;
    }
    // 07d-A5: a second, first-class `AwaitingApproval` source alongside a
    // file-edit proposal — unlike `agent.ask.pending`, resolving this gate
    // genuinely resumes the same turn (the ACP session thread is paused
    // mid-tool-call awaiting exactly this decision).
    if let Ok(ev) = crate::state::append(
        state,
        stream_id,
        "node",
        CHILD_TOOL_PENDING_FAMILY,
        payload,
        vec![],
    ) {
        // 07d-A5 review #5: only set the phase if the CURRENT tracked
        // activity for this agent is still this same turn — an abandoned
        // session (its prompt already failed and was killed,
        // or superseded by a fresh `admit_turn`) must never clobber a
        // newer turn's real, current phase with a gate nobody will ever
        // legitimately decide against that newer turn's context.
        let still_this_turn = crate::state::agent_activity_snapshot(state, &agent_id)
            .is_some_and(|a| a.turn_id == turn_id);
        if still_this_turn {
            crate::state::set_agent_phase(
                state,
                &agent_id,
                crate::state::ActivityPhase::AwaitingApproval {
                    kind: crate::state::ApprovalKind::ChildTool,
                    event_hash: ev.event_hash.clone(),
                },
            );
        }
        return ChildToolAdmission::Pending(ev.event_hash);
    }
    ChildToolAdmission::Deny
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChildToolGateDecisionKind {
    AllowOnce,
    AllowAlways,
    Deny,
}

/// Whether the turn that raised a child-tool request has provably ended — a
/// terminal fact for that turn is on the stream, or this agent has no live
/// activity for it (moved to another turn, stopping, stopped, or none at all
/// because the node restarted since — the parked waiter's own cancel
/// condition). Nothing is parked on such a request, and an AllowOnce
/// recorded against it would linger in `child_tool_allow_once` and silently
/// pre-approve the NEXT, unrelated call of the same kind.
fn gate_turn_has_ended(
    state: &AppState,
    events: &[bakery_core::Event],
    pending: &serde_json::Value,
) -> bool {
    let Some(turn_id) = pending
        .get("turn_id")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return false;
    };
    let terminal = events.iter().any(|e| {
        matches!(
            e.family.as_str(),
            "agent.turn.done"
                | "agent.turn.failed"
                | "agent.turn.cancelled"
                | "agent.turn.exhausted"
        ) && serde_json::from_str::<serde_json::Value>(&e.payload)
            .ok()
            .is_some_and(|p| p.get("turn_id").and_then(serde_json::Value::as_str) == Some(turn_id))
    });
    if terminal {
        return true;
    }
    let Some(agent_id) = pending.get("agent_id").and_then(serde_json::Value::as_str) else {
        return false;
    };
    crate::state::agent_activity_snapshot(state, agent_id).is_none_or(|a| {
        a.turn_id != turn_id
            || a.cancel_requested
            || matches!(
                a.phase,
                crate::state::ActivityPhase::Complete { .. }
                    | crate::state::ActivityPhase::Error { .. }
                    | crate::state::ActivityPhase::Cancelled
            )
    })
}

pub(crate) fn decide_child_tool_gate(
    state: &AppState,
    stream_id: &str,
    event_hash: &str,
    decision: ChildToolGateDecisionKind,
) -> Result<bakery_core::Event, String> {
    let lifecycle = state.lifecycle.lock().unwrap();
    // Shares the serialization boundary used by edit decisions, answers, and
    // Stop. A child-tool Allow submitted after Stop must not leave a durable
    // AllowOnce/AllowAlways capability behind for a cancelled operation.
    let _decision = state.edit_decision_lock.lock().unwrap();

    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map_err(|e| e.to_string())?;
    let pending = events
        .iter()
        .find(|e| e.event_hash == event_hash)
        .ok_or_else(|| format!("no event {event_hash} in stream {stream_id}"))?;
    if pending.family != CHILD_TOOL_PENDING_FAMILY {
        return Err(format!(
            "event {event_hash} is not a pending child-tool gate"
        ));
    }
    let stopped = events.iter().any(|e| {
        e.family == "agent.approval.cancelled" && e.causality.iter().any(|h| h == event_hash)
    });
    if stopped {
        return Err(format!(
            "child-tool gate {event_hash} was stopped before it could be decided"
        ));
    }
    let already_decided = events.iter().any(|e| {
        e.family == CHILD_TOOL_DECIDED_FAMILY && e.causality.iter().any(|h| h == event_hash)
    });
    if already_decided {
        return Err(format!("event {event_hash} already has a gate decision"));
    }
    let pending_payload: serde_json::Value =
        serde_json::from_str(&pending.payload).map_err(|e| e.to_string())?;
    if crate::routes::agent::pending_operation_is_stopping(state, &pending_payload) {
        return Err(format!(
            "child-tool gate {event_hash} was stopped before it could be decided"
        ));
    }
    if gate_turn_has_ended(state, &events, &pending_payload) {
        return Err(format!(
            "permission request {event_hash} expired — the turn that asked for it already ended, so nothing was allowed"
        ));
    }
    let kind: ChildToolKind =
        serde_json::from_value(pending_payload["kind"].clone()).map_err(|e| e.to_string())?;
    let native_reviewable = pending_payload
        .get("native_reviewable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if !matches!(decision, ChildToolGateDecisionKind::Deny)
        && kind.is_workspace_mutation()
        && !native_reviewable
        && !stream_is_task_socket(state, stream_id)
    {
        return Err(format!(
            "{kind:?} can never be approved on this socket — native workspace mutations bypass Manual propose/accept, and only a task socket has a baseline review surface that covers them; deny it instead"
        ));
    }

    // A Baker tool's card (`api_tools`) allows that one call, which its
    // caller runs itself; it never pre-allows a whole kind.
    let one_call = pending_payload.get("bakery_tool").is_some();
    match decision {
        // Allow always on a card that offered a grant gives exactly that
        // grant (grants.rs), with this one call.
        ChildToolGateDecisionKind::AllowAlways if one_call && pending_payload["grant"].is_object() => {
            crate::grants::give_offered(state, stream_id, &pending_payload["grant"])?;
        }
        _ if one_call => {}
        ChildToolGateDecisionKind::AllowOnce => {
            state
                .child_tool_allow_once
                .lock()
                .unwrap()
                .insert((stream_id.to_string(), kind));
        }
        ChildToolGateDecisionKind::AllowAlways => {
            // Exactly what its card offered; a card from before grants had
            // a scope grants where it was asked.
            let grant = serde_json::from_value::<AlwaysGrant>(pending_payload["always"].clone())
                .unwrap_or_else(|_| AlwaysGrant::for_stream(state, stream_id, kind));
            let mut node = state.node.lock().unwrap();
            if !node.always_grants.contains(&grant) {
                node.always_grants.push(grant);
                node.save(&state.base).map_err(|e| e.to_string())?;
            }
        }
        ChildToolGateDecisionKind::Deny => {}
    }

    let payload = serde_json::json!({
        "decided_event": event_hash,
        "decision": decision,
        "turn_id": pending_payload.get("turn_id"),
    })
    .to_string();
    let result = crate::state::append(
        state,
        stream_id,
        "human",
        CHILD_TOOL_DECIDED_FAMILY,
        payload,
        vec![event_hash.to_string()],
    );
    drop(lifecycle);
    if result.is_ok() {
        // Either way, the ACP session resumes after this decision — deny
        // doesn't necessarily end the turn any more than allow does (the
        // model may still try something else, or produce a final reply).
        // `ContactingModel` is the most honest phase available here: ACP's
        // single blocking `chat()` call has no per-step granularity this
        // side can observe, the same limitation `agent_acp_turn` already
        // documents.
        if let Some(agent_id) = pending_payload.get("agent_id").and_then(|v| v.as_str()) {
            crate::state::resolve_awaiting_approval(
                state,
                agent_id,
                event_hash,
                crate::state::ActivityPhase::ContactingModel,
            );
        }
    }
    result
}

// ─── ACP client implementation ───────────────────────────────────────────────

/// Per-string and per-array bound applied to a captured `ToolCall`/
/// `ToolCallUpdate`'s copy in the Heart (STEERING.md "Do not store whole
/// files in Heart events"). Nothing is lost to it: whatever it cuts, the
/// whole payload is kept beside the record and linked from it (`full`).
const MAX_ACP_TOOL_EVENT_STRING: usize = 4000;
const MAX_ACP_TOOL_EVENT_ARRAY: usize = 50;

/// Family for native ACP tool/action evidence — `ToolCall` (announcement)
/// and `ToolCallUpdate` (status/result, including diffs) both land here,
/// distinguished by the `event` field, correlated by `detail.tool_call_id`.
/// Deliberately not `agent.tool.called` (the HTTP tool-loop's family): the
/// payload shapes are genuinely different (ACP's native `ToolCall`/
/// `ToolCallUpdate` structs vs the loop's `{tool,args,ok,step,summary}`),
/// and conflating them would make either consumer guess which shape it got.
pub(crate) const ACP_TOOL_EVENT_FAMILY: &str = "agent.acp.tool_call";

/// One stretch of an agent's thinking (W1-T), in order with its words and
/// tool calls: when it began, how long it took, and the thoughts if its
/// runtime shared any.
pub(crate) const THOUGHT_FAMILY: &str = "agent.thought";
/// The most of a stretch's thoughts History keeps (T-3); the rest is cut.
const THOUGHT_CHARS: usize = 20_000;

/// A native turn's thinking. A stretch starts when the agent last finished
/// something (T-5: its turn began, a step ended, it wrote) and ends when it
/// next answers or acts.
#[derive(Default)]
pub(crate) struct Thinking {
    idle_since: Option<(std::time::Instant, String)>,
    open: Option<Stretch>,
}

pub(crate) struct Stretch {
    started: std::time::Instant,
    started_at: String,
    text: String,
    cut: bool,
}

fn now() -> (std::time::Instant, String) {
    (std::time::Instant::now(), chrono::Utc::now().to_rfc3339())
}

impl Thinking {
    /// A turn begins: nothing open, idle from now.
    fn begin(&mut self) {
        self.open = None;
        self.idle_since = Some(now());
    }

    /// A thought arrives: the stretch opens from when the agent was last
    /// idle. Empty text still opens one: a runtime that thinks without
    /// sharing its thoughts shows the time only (T-4).
    fn thought(&mut self, text: &str) {
        let (started, started_at) = self.idle_since.clone().unwrap_or_else(now);
        let stretch = self.open.get_or_insert_with(|| Stretch { started, started_at, text: String::new(), cut: false });
        let room = THOUGHT_CHARS.saturating_sub(stretch.text.chars().count());
        stretch.cut |= text.chars().count() > room;
        stretch.text.extend(text.chars().take(room));
    }

    /// The agent answered, acted or finished a step: the open stretch ends.
    fn settle(&mut self) -> Option<Stretch> {
        self.idle_since = Some(now());
        self.open.take()
    }
}

impl Stretch {
    fn fact(&self) -> ThoughtFact<'_> {
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        ThoughtFact { started_at: &self.started_at, ms, text: &self.text, cut: self.cut }
    }
}

/// One stretch as History keeps it, from any runtime.
pub(crate) struct ThoughtFact<'a> {
    pub(crate) started_at: &'a str,
    pub(crate) ms: u64,
    /// The thoughts; empty when the runtime thought without sharing them.
    pub(crate) text: &'a str,
    /// Already cut by its runtime's side to [`THOUGHT_CHARS`].
    pub(crate) cut: bool,
}

/// Records a stretch: its thoughts up to [`THOUGHT_CHARS`], cut beyond, or
/// the time only when the runtime didn't share them (T-4).
pub(crate) fn record_thought(state: &AppState, stream_id: &str, seat: Seat, model_id: &str, turn_id: &str, fact: ThoughtFact) {
    let text: String = fact.text.chars().take(THOUGHT_CHARS).collect();
    let cut = fact.cut || text.len() < fact.text.len();
    let shared = !text.is_empty();
    let payload = serde_json::json!({
        "agent_id": crate::node::agent_id(stream_id, seat),
        "turn_id": turn_id,
        "model_id": model_id,
        "started_at": fact.started_at,
        "ms": fact.ms,
        "text": shared.then_some(text),
        "cut": cut,
        "shared": shared,
    });
    let actor = format!("agent:{}", seat.as_str());
    let _ = crate::state::append(state, stream_id, &actor, THOUGHT_FAMILY, payload.to_string(), vec![]);
}
/// A native turn's assistant text, recorded in the order it was written: the
/// run of text before each tool call is one fact, so the transcript can
/// interleave words and tool calls instead of showing the whole reply after
/// every call. `agent.turn.done` still carries the full reply for every other
/// reader; a turn with none of these is rendered from it alone.
pub(crate) const ACP_TEXT_EVENT_FAMILY: &str = "agent.acp.text";

/// What command calls still hold, recorded under their own turn, and the
/// calls gone: every call but `keep`'s turn's (all of them, when a turn
/// ends however it ends: done, failed or stopped).
fn record_held_output(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    commands: &Mutex<BTreeMap<String, NativeCommand>>,
    keep: Option<&str>,
) {
    let gone: Vec<(String, NativeCommand)> = {
        let mut commands = commands.lock().unwrap();
        let ids: Vec<String> = commands.iter().filter(|(_, c)| Some(c.turn_id.as_str()) != keep).map(|(id, _)| id.clone()).collect();
        ids.into_iter().filter_map(|id| commands.remove(&id).map(|c| (id, c))).collect()
    };
    for (call_id, command) in gone.into_iter().filter(|(_, c)| !c.pending.is_empty()) {
        record_acp_tool_event(
            state,
            stream_id,
            seat,
            model_id,
            &command.turn_id,
            "tool_call_update",
            &serde_json::json!({
                "toolCallId": call_id,
                "_meta": {"terminal_output": {"terminal_id": command.terminal_id, "data": command.pending}},
            }),
        );
    }
}

/// Records the text written since the last flush (`flushed` is a byte offset
/// into `text`). A mid-turn flush also ends the paragraph in `text`, so the
/// full reply doesn't run "…to remove.Found it" together, and shares the
/// per-turn event cap with tool events — text it can't record just waits for
/// the next flush. The `last` flush always records whatever is left.
#[allow(clippy::too_many_arguments)]
fn flush_acp_text(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    turn_id: &str,
    text: &Mutex<String>,
    flushed: &AtomicUsize,
    last: bool,
) {
    // Between prompts the buffer still holds the finished turn's reply; a
    // late notification must not re-record it under no turn at all.
    if turn_id.is_empty() {
        return;
    }
    let segment = {
        let mut t = text.lock().unwrap();
        let from = flushed.load(Ordering::Relaxed).min(t.len());
        let segment = t.get(from..).unwrap_or(t.as_str()).to_string();
        if segment.trim().is_empty() {
            return;
        }
        if !last {
            if !t.ends_with('\n') {
                t.push_str("\n\n");
            }
        }
        flushed.store(t.len(), Ordering::Relaxed);
        segment
    };
    let payload = serde_json::json!({
        "agent_id": crate::node::agent_id(stream_id, seat),
        "model_id": model_id,
        "turn_id": turn_id,
        "text": segment,
    })
    .to_string();
    let _ = crate::state::append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        ACP_TEXT_EVENT_FAMILY,
        payload,
        vec![],
    );
}

/// Recursively truncate every string leaf and cap every array's length in
/// a JSON value in place — a generic bound that survives the ACP crate
/// adding fields later (unlike hand-mapping each known field), used to keep
/// [`ACP_TOOL_EVENT_FAMILY`] payloads bounded regardless of what the CLI
/// actually sent. Returns whether anything was cut, so the caller knows the
/// Heart copy is no longer the whole record.
fn bound_json_value(
    v: &mut serde_json::Value,
    max_str: usize,
    max_items: usize,
    depth: u8,
) -> bool {
    if depth > 8 {
        *v = serde_json::Value::String("<nested too deep — truncated>".into());
        return true;
    }
    match v {
        serde_json::Value::String(s) => {
            if s.chars().count() > max_str {
                let truncated: String = s.chars().take(max_str).collect();
                *s = format!("{truncated}… [truncated]");
                return true;
            }
            false
        }
        serde_json::Value::Array(arr) => {
            let mut cut = false;
            if arr.len() > max_items {
                arr.truncate(max_items);
                arr.push(serde_json::Value::String("… [truncated]".into()));
                cut = true;
            }
            for item in arr.iter_mut() {
                cut |= bound_json_value(item, max_str, max_items, depth + 1);
            }
            cut
        }
        serde_json::Value::Object(map) => {
            let mut cut = false;
            for val in map.values_mut() {
                cut |= bound_json_value(val, max_str, max_items, depth + 1);
            }
            cut
        }
        _ => false,
    }
}

/// Held command output goes into a record once it reaches this much, as
/// well as when its time is due: every byte is recorded, and none waits in
/// memory past it.
const HELD_OUTPUT_BYTES: usize = 1024 * 1024;

/// Complete native tool payloads live under the node's cache, outside every
/// room (no sandboxed child is granted it), in a directory named by a hash of
/// the stream id so a stream id never becomes a path component.
fn tool_payload_dir(state: &AppState, stream_id: &str) -> std::path::PathBuf {
    crate::state::cache_root(state)
        .join("tool-payloads")
        .join(&blake3::hash(stream_id.as_bytes()).to_hex()[..32])
}

/// Keep the complete payload when the Heart copy had to be cut. Content-
/// addressed (blake3 of the exact bytes), so a repeat never duplicates and a
/// later read can prove it returns exactly what was stored. `None` when the
/// payload is over the cap or cannot be written — the event then simply
/// carries no `full` reference, never a dangling one.
fn store_full_tool_payload(
    state: &AppState,
    stream_id: &str,
    full: &str,
) -> Option<serde_json::Value> {
    let sha = blake3::hash(full.as_bytes()).to_hex().to_string();
    let dir = tool_payload_dir(state, stream_id);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{sha}.json"));
    if !path.is_file() {
        crate::attachments::write_whole(&path, full.as_bytes()).ok()?;
    }
    Some(serde_json::json!({ "sha": sha, "bytes": full.len() }))
}

/// A file kept whole beside a record, as [`store_full_tool_payload`] keeps
/// a payload: hashed and copied as it streams, never read in whole.
fn store_full_file(state: &AppState, stream_id: &str, file: &Path) -> Option<serde_json::Value> {
    use std::io::Read;
    let mut hasher = blake3::Hasher::new();
    let mut reader = std::fs::File::open(file).ok()?;
    let mut chunk = vec![0u8; 1 << 20];
    let mut bytes = 0u64;
    loop {
        let n = reader.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        hasher.update(&chunk[..n]);
        bytes += n as u64;
    }
    let sha = hasher.finalize().to_hex().to_string();
    let dir = tool_payload_dir(state, stream_id);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{sha}.json"));
    if !path.is_file() {
        let partial = dir.join(format!(".{sha}.{}.partial", std::process::id()));
        if std::fs::copy(file, &partial).and_then(|_| std::fs::rename(&partial, &path)).is_err() {
            let _ = std::fs::remove_file(&partial);
            return None;
        }
    }
    Some(serde_json::json!({ "sha": sha, "bytes": bytes }))
}

/// The complete payload an [`ACP_TOOL_EVENT_FAMILY`] event's `full.sha`
/// names, read back only from this stream's own directory and verified
/// against its hash.
pub(crate) fn read_full_tool_payload(
    state: &AppState,
    stream_id: &str,
    sha: &str,
) -> Result<String, String> {
    if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("not a payload id".to_string());
    }
    let path = tool_payload_dir(state, stream_id).join(format!("{sha}.json"));
    let body = std::fs::read(&path).map_err(|_| "no such payload on this stream".to_string())?;
    if blake3::hash(&body).to_hex().as_str() != sha {
        return Err("stored payload failed its integrity check".to_string());
    }
    // A file kept whole as it was may not be UTF-8 (`store_full_file`).
    Ok(String::from_utf8(body).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// A screenshot's pixels are never kept, however a native runtime echoes its
/// tool result back (an image block, or that block as JSON text): every
/// base64 PNG in a string is cut out.
fn drop_pngs(v: &mut serde_json::Value) {
    const PNG: &str = "iVBORw0KGgo";
    match v {
        serde_json::Value::String(s) => {
            while let Some(start) = s.find(PNG) {
                let len = s[start..]
                    .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')))
                    .unwrap_or(s.len() - start);
                s.replace_range(start..start + len, "(image not kept)");
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(drop_pngs),
        serde_json::Value::Object(map) => map.values_mut().for_each(drop_pngs),
        _ => {}
    }
}

/// Append one native-action Heart event, every one, however many a turn
/// has: a bounded copy in the Heart, the whole payload beside it when the
/// copy was cut.
fn record_acp_tool_event(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    turn_id: &str,
    event: &'static str,
    value: &impl Serialize,
) {
    let Ok(mut detail) = serde_json::to_value(value) else {
        return;
    };
    drop_pngs(&mut detail);
    let full = serde_json::to_string(&detail).unwrap_or_default();
    let cut = bound_json_value(
        &mut detail,
        MAX_ACP_TOOL_EVENT_STRING,
        MAX_ACP_TOOL_EVENT_ARRAY,
        0,
    );
    let mut payload = serde_json::json!({
        "event": event,
        "agent_id": crate::node::agent_id(stream_id, seat),
        "model_id": model_id,
        "turn_id": turn_id,
        "detail": detail,
    });
    if cut {
        if let Some(full_ref) = store_full_tool_payload(state, stream_id, &full) {
            payload["full"] = full_ref;
        }
    }
    let payload = payload.to_string();
    let _ = crate::state::append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        ACP_TOOL_EVENT_FAMILY,
        payload,
        vec![],
    );
}

/// What Stop did to a native tool call it cut short, as the `event` of the
/// call's last [`ACP_TOOL_EVENT_FAMILY`] record: it never ran (held at a
/// permission card or a review that never allowed it), or it had started
/// and its result, or output so far, is what it got done.
pub(crate) const STOPPED_BEFORE: &str = "stopped_before";
pub(crate) const STOPPED_AFTER: &str = "stopped_after";

/// The native calls still open when Stop ended their prompt had started:
/// each is recorded stopped after it ran, once, after the output its command
/// still held (`record_held_output`).
fn record_stopped_calls(state: &AppState, stream_id: &str, seat: Seat, model_id: &str, turn_id: &str, open: &Mutex<BTreeMap<String, String>>) {
    let open = std::mem::take(&mut *open.lock().unwrap());
    for call_id in open.into_keys() {
        let detail = serde_json::json!({ "toolCallId": call_id });
        record_acp_tool_event(state, stream_id, seat, model_id, turn_id, STOPPED_AFTER, &detail);
    }
}

/// Whether Stop has been asked of this agent's turn `turn_id`.
fn stop_asked(state: &AppState, stream_id: &str, seat: Seat, turn_id: &str) -> bool {
    let agent_id = crate::node::agent_id(stream_id, seat);
    !turn_id.is_empty() && crate::state::agent_activity_snapshot(state, &agent_id).is_some_and(|a| a.turn_id == turn_id && a.cancel_requested)
}

/// Live-activity phase for a native ACP tool call. Pure mapping so it can be
/// unit-tested without a running session. `RunningTool` is intentionally
/// transient: the next text delta reverts it to `Streaming`
/// (`append_streaming_text_for_turn`) and the route sets the terminal
/// `Complete`/`Cancelled`, so this only colors the in-flight step. The caller
/// applies it only while the agent's current turn still matches, so a stale or
/// cancelled turn's late notification never clobbers a newer turn's phase.
fn native_tool_running_phase(
    title: &str,
    call_id: &str,
    started_at: String,
    preview: Option<String>,
) -> crate::state::ActivityPhase {
    crate::state::ActivityPhase::RunningTool {
        tool: title.to_string(),
        call_id: call_id.to_string(),
        started_at,
        kind: crate::state::ToolRunKind::Tool,
        preview,
        output: None,
    }
}

/// A one-line preview of a native tool call's input for the live work strip:
/// the shell command for a Terminal/Execute call, else the file path, else a
/// search pattern. `None` when the call carried no inspectable input. This is
/// display only — it never gates or changes what runs.
fn tool_preview(raw_input: Option<&serde_json::Value>) -> Option<String> {
    let input = raw_input?.as_object()?;
    let pick = |key: &str| {
        input
            .get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let raw = pick("command")
        .or_else(|| pick("cmd"))
        .or_else(|| pick("file_path"))
        .or_else(|| pick("path"))
        .or_else(|| pick("pattern"))?;
    Some(one_line(raw))
}

/// Whitespace-collapsed and length-bounded, so the live strip stays one
/// readable line.
fn one_line(raw: &str) -> String {
    let one_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 200;
    if one_line.chars().count() > MAX {
        format!("{}…", one_line.chars().take(MAX).collect::<String>())
    } else {
        one_line
    }
}

/// A native command call — one its runtime reports with `_meta.terminal_info`
/// — while it runs: what the live strip and the studio's Agent terminal show
/// for it, and output Heart has not recorded yet.
struct NativeCommand {
    turn_id: String,
    title: String,
    terminal_id: String,
    /// The whole command, once the runtime has said it: a call's first report
    /// often comes before its input has finished streaming.
    command: Option<String>,
    /// Output since the last record. A runtime may stream output in many
    /// small chunks; one Heart event per chunk would flood the stream, so
    /// they wait here for the update that ends the call.
    pending: String,
    started: std::time::Instant,
    /// Records of held output written while the call ran — each one waits
    /// twice as long as the last, so a long command adds a handful.
    flushes: u32,
}

/// `meta[key][field]` — where the terminal `_meta` keys carry their ids,
/// output and exit code.
fn meta_field<'a>(
    meta: Option<&'a serde_json::Map<String, serde_json::Value>>,
    key: &str,
    field: &str,
) -> Option<&'a serde_json::Value> {
    meta?.get(key)?.get(field)
}

/// A subagent's own words: text chunks the runtime tags with the tool call
/// that launched the subagent, recorded under that call — never in the
/// parent agent's reply.
pub(crate) const ACP_SUBAGENT_TEXT_FAMILY: &str = "agent.acp.subagent_text";
/// Cap on one subagent text record; the runtime keeps its full transcript.
const MAX_SUBAGENT_TEXT_BYTES: usize = 32 * 1024;

/// The tool call a native update belongs to when a subagent made it. Claude's
/// adapter stamps `_meta.claudeCode.parentToolUseId` (the launching Agent or
/// Task call's id) on a subagent's tool calls and text.
fn subagent_parent(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> Option<String> {
    meta_field(meta, "claudeCode", "parentToolUseId")?
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Record a subagent's words gathered so far, under the call that launched it.
fn flush_subagent_text(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    turn_id: &str,
    parent: &str,
    buffers: &Mutex<BTreeMap<String, String>>,
) {
    let Some(text) = buffers.lock().unwrap().remove(parent) else {
        return;
    };
    if turn_id.is_empty() || text.trim().is_empty() {
        return;
    }
    let _ = crate::state::append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        ACP_SUBAGENT_TEXT_FAMILY,
        serde_json::json!({
            "agent_id": crate::node::agent_id(stream_id, seat),
            "model_id": model_id,
            "turn_id": turn_id,
            "parent_tool_call_id": parent,
            "text": text,
        })
        .to_string(),
        vec![],
    );
}

/// Keep `text` up to `MAX_SUBAGENT_TEXT_BYTES` in `buffer`, on a character
/// boundary, marking the cut once.
fn buffer_subagent_text(buffer: &mut String, text: &str) {
    const CUT: &str = " … [truncated]";
    if buffer.ends_with(CUT) {
        return;
    }
    let room = MAX_SUBAGENT_TEXT_BYTES.saturating_sub(buffer.len());
    if text.len() <= room {
        buffer.push_str(text);
        return;
    }
    let end = text
        .char_indices()
        .map(|(at, c)| at + c.len_utf8())
        .take_while(|end| *end <= room)
        .last()
        .unwrap_or(0);
    buffer.push_str(&text[..end]);
    buffer.push_str(CUT);
}

/// The whole command a call's input names (a string, or an argv), as one
/// shell line: for the Agent terminal, and to type for the operator.
fn input_command(raw_input: Option<&serde_json::Value>) -> Option<String> {
    let command = match raw_input?.get("command")? {
        serde_json::Value::String(command) => command.trim().to_string(),
        serde_json::Value::Array(argv) => crate::tools::shell_line(
            &argv.iter().filter_map(|a| a.as_str().map(str::to_string)).collect::<Vec<_>>(),
        ),
        _ => return None,
    };
    (!command.is_empty()).then_some(command)
}

struct GateClient {
    state: AppState,
    stream_id: String,
    seat: Seat,
    generation: u64,
    /// The seat's assigned model id (e.g. `"acp"` or a named plug's own id
    /// like `"acp:codex"`) — carried into both the child-tool gate's `plug`
    /// field and every [`ACP_TOOL_EVENT_FAMILY`] event's `model_id`.
    model_id: String,
    text: Arc<Mutex<String>>,
    /// Byte offset into `text` already recorded as [`ACP_TEXT_EVENT_FAMILY`]
    /// facts this turn — reset with `text` at the top of every prompt.
    text_flushed: Arc<AtomicUsize>,
    /// The current prompt's `turn_id` (07d-A5 review #5) — set by
    /// `session_main` from its `AcpPromptMsg` at the top of every prompt,
    /// so a gate/tool-call callback landing during
    /// this prompt always sees the turn actually in flight, never a stale
    /// one from a prompt that already finished.
    current_turn_id: Arc<Mutex<String>>,
    current_delta_tx: Arc<Mutex<Option<std::sync::mpsc::Sender<String>>>>,
    shared: Arc<Mutex<AcpSessionShared>>,
    /// True for non-Burn streams: auto-approve all native ACP tools so the
    /// child can read, edit, run, and delete freely like the human PTY.
    trusted: bool,
    /// This turn's command calls still running, by tool call id (shared
    /// with `session_main`, which records what they hold when a turn ends).
    commands: Arc<Mutex<BTreeMap<String, NativeCommand>>>,
    /// This turn's subagent tool calls → the call that launched their
    /// subagent. A later update without the stamp still belongs there.
    subagent_calls: Arc<Mutex<BTreeMap<String, String>>>,
    /// Subagent words not yet recorded, by the call that launched them.
    subagent_text: Arc<Mutex<BTreeMap<String, String>>>,
    /// This turn's tool calls still open → their titles, so a call that ends
    /// out of order still lands in the live strip's finished list by name.
    tool_titles: Arc<Mutex<BTreeMap<String, String>>>,
    /// This turn's thinking: the stretch open now (W1-T).
    thinking: Arc<Mutex<Thinking>>,
    /// A native compaction's summary as it streams in, by its id.
    compactions: Mutex<BTreeMap<String, String>>,
}

/// The runtime's own option for a decision — always its "once" variant when
/// it offers one. Bakery remembers "always" itself (node-wide, visible and
/// revocable in Settings); a runtime told "always" may save the rule into the
/// project's or its own config files (`.claude/settings.local.json`), state
/// Bakery neither shows nor controls.
fn permission_outcome(
    options: &[acp::PermissionOption],
    decision: ChildToolGateDecisionKind,
) -> acp::RequestPermissionOutcome {
    let wanted = match decision {
        ChildToolGateDecisionKind::AllowOnce | ChildToolGateDecisionKind::AllowAlways => [
            acp::PermissionOptionKind::AllowOnce,
            acp::PermissionOptionKind::AllowAlways,
        ],
        ChildToolGateDecisionKind::Deny => [
            acp::PermissionOptionKind::RejectOnce,
            acp::PermissionOptionKind::RejectAlways,
        ],
    };
    wanted
        .iter()
        .find_map(|kind| options.iter().find(|option| option.kind == *kind))
        .map(|option| {
            acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                option.option_id.clone(),
            ))
        })
        .unwrap_or(acp::RequestPermissionOutcome::Cancelled)
}

pub(crate) fn child_tool_gate_decision(
    state: &AppState,
    stream_id: &str,
    pending_hash: &str,
) -> Option<ChildToolGateDecisionKind> {
    state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .ok()?
        .into_iter()
        .find(|event| {
            (event.family == CHILD_TOOL_DECIDED_FAMILY
                || event.family == "agent.approval.cancelled")
                && event.causality.iter().any(|hash| hash == pending_hash)
        })
        .and_then(|event| {
            if event.family == "agent.approval.cancelled" {
                Some(ChildToolGateDecisionKind::Deny)
            } else {
                serde_json::from_str::<serde_json::Value>(&event.payload)
                    .ok()?
                    .get("decision")
                    .cloned()
                    .and_then(|value| serde_json::from_value(value).ok())
            }
        })
}

/// A released ACP adapter normally has no way to carry human-edited bytes
/// through `session/request_permission`: ACP itself only selects an option.
/// The pinned Claude adapter patch advertises and uses this narrow extension
/// for `Edit`, `Write` and `NotebookEdit`. Treat it as unavailable unless the
/// exact request marker is present, so another ACP runtime cannot
/// accidentally receive an opaque Bakery payload.
#[derive(Debug, Clone)]
struct NativeReviewRequest {
    tool_call_id: String,
    tool_name: String,
    file_path: String,
    old_content: String,
    proposed_content: String,
    /// The proposal shows the whole file (else only the change: a notebook
    /// cell, or the edit itself in a file too large to keep whole).
    whole: bool,
    /// The whole old file, kept beside the record when too large for it.
    old_full: Option<serde_json::Value>,
    input: serde_json::Value,
}

/// A file as Claude Code reads it to edit it: UTF-16 when it opens with that
/// mark, else UTF-8 (a byte that isn't shows as U+FFFD, as there), with
/// `\r\n` read as `\n`. A review compares and replaces in this view, as
/// Claude Code's own Edit does; Claude Code writes it back in the file's own
/// encoding and line ends.
fn as_claude_reads(bytes: &[u8]) -> String {
    let text = if bytes.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    };
    text.replace("\r\n", "\n")
}

/// The text Claude Code's Edit replaces for `old`: `old` itself, or, as it
/// finds one, the same text with curly quotes where `old` has straight ones.
fn find_actual(content: &str, old: &str) -> Option<String> {
    if content.contains(old) {
        return Some(old.to_string());
    }
    let straight = |c: char| match c {
        '\u{2018}' | '\u{2019}' => '\'',
        '\u{201c}' | '\u{201d}' => '"',
        c => c,
    };
    // ponytail: a char-by-char scan, only once the plain search missed.
    let hay: Vec<char> = content.chars().collect();
    let flat: Vec<char> = hay.iter().map(|&c| straight(c)).collect();
    let needle: Vec<char> = old.chars().map(straight).collect();
    let at = flat.windows(needle.len()).position(|window| window == needle.as_slice())?;
    Some(hay[at..at + needle.len()].iter().collect())
}

/// Whether `path` names something in one of `folders`, with no `..` to climb
/// out of it.
fn in_folders(path: &str, folders: &[PathBuf]) -> bool {
    let path = Path::new(path);
    if !path.is_absolute() || path.components().any(|part| part == std::path::Component::ParentDir) {
        return false;
    }
    // As written, and as its links resolve: a link inside to a file outside
    // is no file inside, and one that won't resolve (dangling) isn't either.
    let Some(real) = resolved(path) else {
        return false;
    };
    let inside = |p: &Path| folders.iter().any(|folder| p.starts_with(folder) || resolved(folder).is_some_and(|f| p.starts_with(f)));
    inside(path) && inside(&real)
}

/// `path` with its deepest existing part's links resolved, the rest as
/// written; `None` when an existing part won't resolve (a dangling link).
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut rest = Vec::new();
    let mut at = path;
    loop {
        if std::fs::symlink_metadata(at).is_ok() {
            let real = at.canonicalize().ok()?;
            return Some(rest.iter().rev().fold(real, |acc, piece: &&std::ffi::OsStr| acc.join(piece)));
        }
        match (at.file_name(), at.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                at = parent;
            }
            _ => return Some(path.to_path_buf()),
        }
    }
}

/// What a review shows for one native write: the file before and after, or
/// only the part that changes (`whole: false`).
struct ReviewView {
    old: String,
    proposed: String,
    whole: bool,
}

/// How much of a file too large to keep whole a review of a Write shows.
const REVIEW_HEAD_BYTES: usize = 64 * 1024;
/// A file past this isn't reviewed at all: refused, with the reason.
const REVIEW_CEILING_BYTES: u64 = 1 << 30;

/// A file as a review reads it, and its whole size: all of it up to the
/// bound a record keeps whole (a notebook always), else only the head the
/// review shows; nothing past the ceiling.
pub(crate) fn read_for_review(path: &Path, tool: &str, file_path: &str) -> Result<(Vec<u8>, u64), String> {
    let size = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), 0)),
        Err(e) => return Err(format!("read {file_path}: {e}")),
    };
    if size > REVIEW_CEILING_BYTES {
        return Err(format!(
            "{file_path} is {} MB, past the {} MB Review reads: change it with a command instead",
            size >> 20,
            REVIEW_CEILING_BYTES >> 20
        ));
    }
    let read = if tool == "NotebookEdit" || size <= crate::tools::MAX_EDIT_BYTES as u64 {
        std::fs::read(path)
    } else {
        use std::io::Read;
        let mut head = Vec::new();
        std::fs::File::open(path).and_then(|file| file.take(REVIEW_HEAD_BYTES as u64 + 4).read_to_end(&mut head)).map(|_| head)
    };
    read.map(|bytes| (bytes, size)).map_err(|e| format!("read {file_path}: {e}"))
}

/// The review of `tool`'s `input` against the file's current `bytes`, as
/// Claude Code would apply it, or why it cannot apply.
fn review_view(
    tool: &str,
    input: &serde_json::Map<String, serde_json::Value>,
    bytes: &[u8],
    size: u64,
    file_path: &str,
) -> Result<ReviewView, String> {
    let field = |name: &str| {
        input
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("native {tool} request omitted {name}"))
    };
    let old = as_claude_reads(bytes);
    let big = size > crate::tools::MAX_EDIT_BYTES as u64;
    match tool {
        "Write" => {
            let proposed = field("content")?.to_string();
            if !big {
                return Ok(ReviewView { old, proposed, whole: true });
            }
            let mut cut = REVIEW_HEAD_BYTES.min(old.len());
            while !old.is_char_boundary(cut) {
                cut -= 1;
            }
            let more = size.saturating_sub(cut as u64);
            let head = format!("{}\n[… {more} more bytes of {file_path} not shown]", &old[..cut]);
            Ok(ReviewView { old: head, proposed, whole: false })
        }
        "Edit" => {
            let old_string = field("old_string")?;
            let new_string = field("new_string")?;
            if old_string.is_empty() {
                // Claude Code makes a new file this way, or fills an empty one.
                if !old.is_empty() {
                    return Err(format!("{file_path} already has content: name the text to replace in it"));
                }
                return Ok(ReviewView { old, proposed: new_string.to_string(), whole: true });
            }
            if big {
                return Ok(ReviewView { old: old_string.to_string(), proposed: new_string.to_string(), whole: false });
            }
            let actual = find_actual(&old, old_string)
                .ok_or_else(|| format!("{file_path} does not contain the text to replace: read it again"))?;
            let replace_all = input.get("replace_all").and_then(serde_json::Value::as_bool) == Some(true);
            let count = old.matches(actual.as_str()).count();
            if count > 1 && !replace_all {
                return Err(format!(
                    "the text to replace occurs {count} times in {file_path}: include more of its surroundings to pick one, or set replace_all"
                ));
            }
            let proposed = if replace_all { old.replace(&actual, new_string) } else { old.replacen(&actual, new_string, 1) };
            Ok(ReviewView { old, proposed, whole: true })
        }
        "NotebookEdit" => {
            let notebook: serde_json::Value =
                serde_json::from_str(&old).map_err(|_| format!("{file_path} is not a valid notebook"))?;
            let cells = notebook
                .get("cells")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| format!("{file_path} has no cells"))?;
            // The cell its id names, else `cell-N`'s Nth, as Claude Code finds it.
            let index = input.get("cell_id").and_then(serde_json::Value::as_str).and_then(|id| {
                cells
                    .iter()
                    .position(|cell| cell.get("id").and_then(serde_json::Value::as_str) == Some(id))
                    .or_else(|| {
                        id.strip_prefix("cell-")
                            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
                            .and_then(|n| n.parse().ok())
                    })
            });
            let source = |at: usize| {
                cells.get(at).map(|cell| match cell.get("source") {
                    Some(serde_json::Value::Array(lines)) => {
                        lines.iter().filter_map(serde_json::Value::as_str).collect::<String>()
                    }
                    Some(serde_json::Value::String(text)) => text.clone(),
                    _ => String::new(),
                })
            };
            let new_source = field("new_source")?.to_string();
            let (old, proposed) = match input.get("edit_mode").and_then(serde_json::Value::as_str) {
                Some("insert") => (String::new(), new_source),
                Some("delete") => (
                    index.and_then(source).ok_or_else(|| format!("{file_path} has no such cell"))?,
                    String::new(),
                ),
                // Past the last cell, Claude Code adds one.
                _ => (index.and_then(source).unwrap_or_default(), new_source),
            };
            Ok(ReviewView { old, proposed, whole: false })
        }
        _ => Err(format!("{tool} is not a reviewed write")),
    }
}

/// What a native review's proposal showed as the file before, recomputed
/// from the file's `bytes` now: its `old_content` while the file has not
/// changed under the proposal.
pub(crate) fn native_review_baseline(operation: &serde_json::Value, path: &Path, file_path: &str) -> Result<String, String> {
    let tool = operation.get("tool_name").and_then(serde_json::Value::as_str).unwrap_or_default();
    let input = operation
        .get("input")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "native review proposal has no tool input".to_string())?;
    let (bytes, size) = read_for_review(path, tool, file_path)?;
    review_view(tool, input, &bytes, size, file_path).map(|view| view.old)
}

fn native_review_marker(request: &acp::RequestPermissionRequest) -> Option<&serde_json::Value> {
    request
        .meta
        .as_ref()
        .and_then(|meta| meta.get(NATIVE_REVIEW_META_KEY))
        .filter(|marker| {
            marker.get("version").and_then(serde_json::Value::as_u64) == Some(NATIVE_REVIEW_VERSION)
                && matches!(
                    marker.get("toolName").and_then(serde_json::Value::as_str),
                    Some("Edit" | "Write" | "NotebookEdit")
                )
        })
}

/// A reviewed write as a Review proposal, or `None` when it is not one: no
/// review marker, or a write into the seat's own temp folder (`seat_tmp`),
/// which takes the ordinary gate as any other runtime's writes do.
fn native_review_request(
    state: &AppState,
    stream_id: &str,
    request: &acp::RequestPermissionRequest,
    seat_tmp: &[PathBuf],
) -> Result<Option<NativeReviewRequest>, String> {
    let Some(marker) = native_review_marker(request) else {
        return Ok(None);
    };
    let tool_name = marker
        .get("toolName")
        .and_then(serde_json::Value::as_str)
        .expect("native review marker was validated")
        .to_string();
    let input = request
        .tool_call
        .fields
        .raw_input
        .clone()
        .ok_or_else(|| "native review request omitted its tool input".to_string())?;
    let input_object = input
        .as_object()
        .ok_or_else(|| "native review request had a non-object tool input".to_string())?;
    let path_key = if tool_name == "NotebookEdit" { "notebook_path" } else { "file_path" };
    let requested_path = input_object
        .get(path_key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("native review request omitted {path_key}"))?;
    if in_folders(requested_path, seat_tmp) {
        return Ok(None);
    }
    let jail = crate::state::jail_for(state, stream_id)?;
    // Never a credential, a runtime's own settings or an always-blocked
    // path: no review item (and so none of its content) is ever made.
    let resolved = jail.resolve_write(requested_path)?;
    if let Some(blocked) = crate::machine_access::always_blocked(state, &resolved, jail.root()) {
        return Err(format!(
            "{} is always blocked ({}): never work around it",
            resolved.display(),
            blocked.display()
        ));
    }
    let file_path = resolved
        .strip_prefix(jail.root())
        .map_err(|_| "native review path escaped its workspace".to_string())?
        .to_string_lossy()
        .into_owned();
    // No concurrent-writer guard here: Claude reads outside the Bakery, so
    // the guard never sees it read the file again, and Claude Code refuses a
    // stale edit itself. Review shows the change against the file as it is.
    let (bytes, size) = read_for_review(&resolved, &tool_name, &file_path)?;
    let view = review_view(&tool_name, input_object, &bytes, size, &file_path)?;
    // Too large to keep whole in the record: the whole is kept beside it,
    // copied as it streams, never read in.
    let old_full = (size > crate::tools::MAX_EDIT_BYTES as u64).then(|| store_full_file(state, stream_id, &resolved)).flatten();
    Ok(Some(NativeReviewRequest {
        tool_call_id: request.tool_call.tool_call_id.0.to_string(),
        tool_name,
        file_path,
        old_content: view.old,
        proposed_content: view.proposed,
        whole: view.whole,
        old_full,
        input,
    }))
}

/// A reviewed write refused, with the reason Claude reads in place of the
/// adapter's generic one (when the adapter takes reasons).
fn review_refusal(
    options: &[acp::PermissionOption],
    tool_call_id: &str,
    reason: &str,
    reasons: bool,
) -> acp::RequestPermissionResponse {
    let response = acp::RequestPermissionResponse::new(permission_outcome(options, ChildToolGateDecisionKind::Deny));
    if !reasons {
        return response;
    }
    let mut meta = serde_json::Map::new();
    meta.insert(
        NATIVE_REVIEW_META_KEY.to_string(),
        serde_json::json!({
            "version": NATIVE_REVIEW_VERSION,
            "toolCallId": tool_call_id,
            "reason": reason,
        }),
    );
    response.meta(meta)
}

/// A reviewed write accepted with `final_content`: Claude's own input when
/// that is what it proposed, so Claude Code applies its own edit exactly;
/// else the operator's content in place of what Claude proposed.
fn native_review_response(
    options: &[acp::PermissionOption],
    review: &NativeReviewRequest,
    final_content: &str,
) -> acp::RequestPermissionResponse {
    let Some(allow) = options
        .iter()
        .find(|option| option.kind == acp::PermissionOptionKind::AllowOnce)
    else {
        return acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
    };
    let mut input = review.input.clone();
    let Some(object) = input.as_object_mut() else {
        return acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled);
    };
    let text = |content: &str| serde_json::Value::String(content.to_string());
    if final_content != review.proposed_content {
        match review.tool_name.as_str() {
            "Write" => {
                object.insert("content".to_string(), text(final_content));
            }
            "Edit" if review.whole => {
                // Replacing the complete reviewed baseline makes edited
                // acceptance exact, even when the original edit changed only
                // one small range.
                object.insert("old_string".to_string(), text(&review.old_content));
                object.insert("new_string".to_string(), text(final_content));
                if object.contains_key("replace_all") {
                    object.insert("replace_all".to_string(), serde_json::Value::Bool(false));
                }
            }
            // Only the change was shown: the operator's version replaces it.
            "Edit" => {
                object.insert("new_string".to_string(), text(final_content));
            }
            "NotebookEdit" => {
                object.insert("new_source".to_string(), text(final_content));
                // A cell the operator rewrote instead of deleting stays.
                if object.get("edit_mode").and_then(serde_json::Value::as_str) == Some("delete") {
                    object.insert("edit_mode".to_string(), text("replace"));
                }
            }
            _ => return acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled),
        }
    }
    let mut meta = serde_json::Map::new();
    meta.insert(
        NATIVE_REVIEW_META_KEY.to_string(),
        serde_json::json!({
            "version": NATIVE_REVIEW_VERSION,
            "toolCallId": review.tool_call_id,
            "updatedInput": input,
        }),
    );
    acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Selected(
        acp::SelectedPermissionOutcome::new(allow.option_id.clone()),
    ))
    .meta(meta)
}

fn native_review_decision(
    state: &AppState,
    stream_id: &str,
    pending_hash: &str,
) -> Option<Result<String, String>> {
    let events = state.heart.lock().unwrap().read_stream(stream_id).ok()?;
    for event in events.iter().rev() {
        if !event.causality.iter().any(|hash| hash == pending_hash) {
            continue;
        }
        match event.family.as_str() {
            "agent.edit.accepted" => {
                let payload: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
                return payload
                    .get("new_content")
                    .and_then(serde_json::Value::as_str)
                    .map(|content| Ok(content.to_string()));
            }
            "agent.edit.rejected" => return Some(Err("The operator rejected this change in Review.".to_string())),
            // An acceptance that could not stand (the file changed under the
            // proposal): Claude hears why.
            "agent.edit.failed" => {
                let payload: serde_json::Value = serde_json::from_str(&event.payload).ok()?;
                let why = payload.get("reason").and_then(serde_json::Value::as_str).unwrap_or("it failed");
                return Some(Err(format!("Review could not accept this change: {why}")));
            }
            // Stop closes the Review card before the native permission loop
            // wakes. Treat it as a cancellation, never as a synthetic human
            // rejection that could let the session keep working.
            "agent.approval.cancelled" => return Some(Err("The operator stopped this turn.".to_string())),
            _ => {}
        }
    }
    None
}

fn project_command(command: &acp::AvailableCommand) -> AcpCommand {
    let input_hint = command.input.as_ref().and_then(|input| {
        serde_json::to_value(input)
            .ok()?
            .get("hint")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    AcpCommand {
        name: command.name.clone(),
        description: command.description.clone(),
        input_hint,
    }
}

fn project_auth_status(params: &serde_json::Value) -> Option<AcpAuthStatus> {
    let status = params.get("authStatus").unwrap_or(params);
    let kind = status.get("kind")?.as_str()?.to_string();
    let clean = |name: &str| {
        status
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let plan = clean("plan").or_else(|| {
        status
            .get("account")
            .and_then(|account| account.get("plan"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    });
    Some(AcpAuthStatus {
        kind,
        label: clean("label"),
        detail: clean("detail"),
        plan,
    })
}

impl GateClient {
    /// The agent answers, acts or ends a step: its open stretch of thinking
    /// (W1-T) lands in Heart, after the words it wrote before thinking.
    fn settle_thinking(&self) {
        let open = self.thinking.lock().unwrap().settle();
        if let Some(stretch) = open {
            let turn_id = self.current_turn_id.lock().unwrap().clone();
            self.flush_text(&turn_id);
            record_thought(&self.state, &self.stream_id, self.seat, &self.model_id, &turn_id, stretch.fact());
        }
    }

    /// A tool call or permission request follows: the text before it lands
    /// in Heart first.
    fn flush_text(&self, turn_id: &str) {
        flush_acp_text(
            &self.state,
            &self.stream_id,
            self.seat,
            &self.model_id,
            turn_id,
            &self.text,
            &self.text_flushed,
            false,
        );
    }

    /// The subagent `call_id` belongs to: from the update's own stamp, or
    /// from the call that first named it.
    fn subagent_of(
        &self,
        call_id: &str,
        meta: Option<&serde_json::Map<String, serde_json::Value>>,
    ) -> Option<String> {
        subagent_parent(meta).or_else(|| self.subagent_calls.lock().unwrap().get(call_id).cloned())
    }

    fn flush_subagent_text(&self, turn_id: &str, parent: &str) {
        flush_subagent_text(
            &self.state,
            &self.stream_id,
            self.seat,
            &self.model_id,
            turn_id,
            parent,
            &self.subagent_text,
        );
    }

    /// Writes `text` to this stream's Agent terminal in the studio, as part
    /// of command call `call_id`'s block. Parallel calls report output in any
    /// order, and other seats and the `run` tool write there too, so the
    /// terminal itself keeps whose block is last and each call's output
    /// carries its `$ command` header (see `agent_terminal_write`). A call
    /// whose command is not said yet has no header to print, so it writes
    /// nothing. An empty `text` just opens the block.
    fn echo(&self, call_id: &str, command: Option<&str>, text: &str, own_line: bool) {
        let Some(command) = command else { return };
        crate::terminals::agent_terminal_write(
            &self.state,
            &self.stream_id,
            call_id,
            command,
            text,
            own_line,
        );
    }

    /// A call Stop caught at a permission card or a review: it never ran.
    /// Recorded with what the request says of it, so it reads whole even when
    /// the runtime never announced it, and it leaves the open calls: the
    /// prompt's end must not call it started.
    fn stopped_before(&self, turn_id: &str, tool_call: &acp::ToolCallUpdate) {
        self.tool_titles.lock().unwrap().remove(&tool_call.tool_call_id.0.to_string());
        record_acp_tool_event(&self.state, &self.stream_id, self.seat, &self.model_id, turn_id, STOPPED_BEFORE, tool_call);
    }

    /// Starts following a native command call, when `tool_call` is one.
    fn start_command(&self, turn_id: &str, tool_call: &acp::ToolCall) {
        let Some(terminal_id) = meta_field(tool_call.meta.as_ref(), "terminal_info", "terminal_id")
            .and_then(serde_json::Value::as_str)
        else {
            return;
        };
        let call_id = tool_call.tool_call_id.0.to_string();
        let command = input_command(tool_call.raw_input.as_ref());
        self.echo(&call_id, command.as_deref(), "", true);
        self.commands.lock().unwrap().insert(
            call_id,
            NativeCommand {
                turn_id: turn_id.to_string(),
                title: tool_call.title.clone(),
                terminal_id: terminal_id.to_string(),
                command,
                pending: String::new(),
                started: std::time::Instant::now(),
                flushes: 0,
            },
        );
    }

    /// Folds one update into its command call, when it belongs to one. The
    /// live strip and the Agent terminal follow the call as it goes; its
    /// output waits in [`NativeCommand::pending`], and the update that ends
    /// the call carries all of it into Heart beside the exit code. Returns
    /// true when the update held only output, which needs no record of its
    /// own.
    fn follow_command(&self, turn_id: &str, update: &mut acp::ToolCallUpdate) -> bool {
        let call_id = update.tool_call_id.0.to_string();
        // A call left over from an earlier turn will never end here: what it
        // held is recorded, then it goes.
        record_held_output(&self.state, &self.stream_id, self.seat, &self.model_id, &self.commands, Some(turn_id));
        let mut commands = self.commands.lock().unwrap();
        let Some(command) = commands.get_mut(&call_id) else {
            return false;
        };
        let mut news = false;
        if command.command.is_none() {
            command.command = input_command(update.fields.raw_input.as_ref());
            self.echo(&call_id, command.command.as_deref(), "", true);
            news = command.command.is_some();
        }
        let chunk = ["terminal_output_delta", "terminal_output"]
            .into_iter()
            .find_map(|key| update.meta.as_mut()?.remove(key));
        if update.meta.as_ref().is_some_and(|meta| meta.is_empty()) {
            update.meta = None;
        }
        let data = chunk
            .as_ref()
            .and_then(|chunk| chunk.get("data"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let mut newest_line = None;
        if !data.is_empty() {
            let due = Duration::from_secs(1 << command.flushes.min(16));
            let large = command.pending.len() >= HELD_OUTPUT_BYTES;
            if !command.pending.is_empty() && (large || command.started.elapsed() >= due) {
                let held = std::mem::take(&mut command.pending);
                record_acp_tool_event(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    &self.model_id,
                    turn_id,
                    "tool_call_update",
                    &serde_json::json!({
                        "toolCallId": call_id,
                        "_meta": {"terminal_output": {"terminal_id": command.terminal_id, "data": held}},
                    }),
                );
                command.flushes += 1;
            }
            self.echo(&call_id, command.command.as_deref(), data, false);
            command.pending.push_str(data);
            // From the held output, not the chunk: a line can arrive in pieces.
            newest_line = command
                .pending
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(one_line);
            news |= newest_line.is_some();
        }
        let exit = meta_field(update.meta.as_ref(), "terminal_exit", "exit_code")
            .and_then(serde_json::Value::as_i64);
        let failed = update.fields.status == Some(acp::ToolCallStatus::Failed);
        if exit.is_some() || failed || update.fields.status == Some(acp::ToolCallStatus::Completed) {
            let Some(command) = commands.remove(&call_id) else {
                return false;
            };
            if !command.pending.is_empty() {
                update.meta.get_or_insert_with(Default::default).insert(
                    "terminal_output".to_string(),
                    serde_json::json!({"terminal_id": command.terminal_id, "data": command.pending}),
                );
            }
            let end = match exit {
                Some(code) => format!("[exit {code}]\n"),
                None if failed => "[failed]\n".to_string(),
                None => String::new(),
            };
            self.echo(&call_id, command.command.as_deref(), &end, true);
            return false;
        }
        self.show_running_command(turn_id, &call_id, command, newest_line, news);
        chunk.is_some() && update.meta.is_none() && update.fields == Default::default()
    }

    /// Shows a command call as the running step — its real command and its
    /// newest output line — while it is still this turn's and nothing more
    /// pressing (an approval) is on screen. Also brings it back after an
    /// approval, when the strip had moved on to contacting the model.
    fn show_running_command(
        &self,
        turn_id: &str,
        call_id: &str,
        command: &NativeCommand,
        newest_line: Option<String>,
        news: bool,
    ) {
        use crate::state::ActivityPhase;
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        let Some(activity) = crate::state::agent_activity_snapshot(&self.state, &agent_id) else {
            return;
        };
        if activity.turn_id != turn_id || activity.cancel_requested {
            return;
        }
        let (started_at, output) = match activity.phase {
            ActivityPhase::RunningTool {
                call_id: running,
                started_at,
                output,
                ..
            } if running == call_id => {
                if !news {
                    return;
                }
                (started_at, newest_line.or(output))
            }
            ActivityPhase::AwaitingApproval { .. }
            | ActivityPhase::Complete { .. }
            | ActivityPhase::Error { .. }
            | ActivityPhase::Cancelled => return,
            _ => (chrono::Utc::now().to_rfc3339(), newest_line),
        };
        crate::state::set_agent_phase(
            &self.state,
            &agent_id,
            ActivityPhase::RunningTool {
                tool: command.title.clone(),
                call_id: call_id.to_string(),
                started_at,
                kind: crate::state::ToolRunKind::Tool,
                preview: command.command.as_deref().map(one_line),
                output,
            },
        );
    }

    /// An ACP form elicitation — how Claude's AskUserQuestion arrives. Its
    /// questions become the same `agent.ask.pending` an HTTP seat's `ask_user`
    /// records, the turn parks on the human's answer like any approval, and
    /// the answer goes back as the form's content. Stop cancels it; so does
    /// the runtime withdrawing the request.
    async fn elicit(
        &self,
        params: &serde_json::Value,
        cancellation: &RequestCancellation,
    ) -> acp::Result<serde_json::Value> {
        let cancel = serde_json::json!({ "action": "cancel" });
        if params.get("mode").and_then(|m| m.as_str()) != Some("form") {
            return Ok(serde_json::json!({ "action": "decline" }));
        }
        if crate::learning::scratch(&self.state, &self.stream_id) {
            crate::learning::refuse(&self.state, &self.stream_id, "a question for the operator");
            return Ok(serde_json::json!({ "action": "decline" }));
        }
        let message = params.get("message").and_then(|m| m.as_str()).unwrap_or("");
        let Some(questions) = params
            .get("requestedSchema")
            .and_then(|schema| crate::ask::questions_from_elicitation(message, schema))
        else {
            return Ok(serde_json::json!({ "action": "decline" }));
        };
        let turn_id = self.current_turn_id.lock().unwrap().clone();
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        let still_this_turn = || {
            acp_generation_is_current(&self.state, &self.stream_id, self.seat, self.generation)
                && crate::state::agent_activity_snapshot(&self.state, &agent_id)
                    .is_some_and(|a| a.turn_id == turn_id && !a.cancel_requested)
        };
        if turn_id.is_empty() || !still_this_turn() {
            return Ok(cancel);
        }
        self.flush_text(&turn_id);
        let payload = serde_json::json!({
            "questions": questions,
            "status": "pending",
            "agent_id": agent_id,
            "model_id": self.model_id,
            "turn_id": turn_id,
            "tool_call_id": params.get("toolCallId"),
        });
        let Ok(asked) = crate::state::append(
            &self.state,
            &self.stream_id,
            &format!("agent:{}", self.seat.as_str()),
            "agent.ask.pending",
            payload.to_string(),
            vec![],
        ) else {
            return Ok(cancel);
        };
        crate::state::set_agent_phase(
            &self.state,
            &agent_id,
            crate::state::ActivityPhase::AwaitingApproval {
                kind: crate::state::ApprovalKind::Question,
                event_hash: asked.event_hash.clone(),
            },
        );
        // Registered so the answer route knows a live turn resumes; the wake
        // is advisory, and Heart is read again every couple of seconds.
        let waiter = crate::state::register_decision_waiter(&self.state, &asked.event_hash);
        let mut tick = 0u32;
        let reply = loop {
            if !still_this_turn() {
                break Ok(cancel);
            }
            if waiter.try_recv().is_ok() || tick.is_multiple_of(40) {
                match crate::ask::outcome(&self.state, &self.stream_id, &asked.event_hash, &questions) {
                    Some(crate::ask::AskOutcome::Answered(answers)) => {
                        break Ok(crate::ask::elicitation_reply(&questions, &answers))
                    }
                    Some(crate::ask::AskOutcome::Cancelled) => break Ok(cancel),
                    None => {}
                }
            }
            if cancellation.is_cancelled() {
                withdraw_pending_approval(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    &turn_id,
                    &asked.event_hash,
                    "question",
                );
                break Err(acp::Error::request_cancelled());
            }
            tick = tick.wrapping_add(1);
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        crate::state::unregister_decision_waiter(&self.state, &asked.event_hash);
        reply
    }

    async fn request_native_review_permission(
        &self,
        request: &acp::RequestPermissionRequest,
        review: NativeReviewRequest,
        operator_mode: Option<crate::agent_control::OperatingMode>,
        cancellation: &RequestCancellation,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let reasons = self.shared.lock().unwrap().review_reasons;
        let refuse = |reason: &str| Ok(review_refusal(&request.options, &review.tool_call_id, reason, reasons));
        // Plan must remain structurally read-only. Return the adapter's own
        // rejection option; do not create a decorative proposal that could
        // later be accepted after the turn has moved on.
        if operator_mode == Some(crate::agent_control::OperatingMode::Plan) {
            return refuse("The thread is in Plan mode: it changes no files until the operator takes it out of Plan.");
        }
        if !request
            .options
            .iter()
            .any(|option| option.kind == acp::PermissionOptionKind::AllowOnce)
        {
            return refuse("This request offered no way to allow it.");
        }
        let turn_id = self.current_turn_id.lock().unwrap().clone();
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        let live_turn = !turn_id.is_empty()
            && crate::state::agent_activity_snapshot(&self.state, &agent_id)
                .is_some_and(|activity| activity.turn_id == turn_id && !activity.cancel_requested);
        // A direct Model::chat caller has no Review/turn lifecycle to resume.
        // Do not leave it with an orphaned proposal.
        if !live_turn {
            return Ok(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Cancelled,
            ));
        }
        let title = request
            .tool_call
            .fields
            .title
            .clone()
            .unwrap_or_else(|| format!("{} {}", review.tool_name, review.file_path));
        let proposal = match crate::routes::agent::propose_native_edit_event(
            &self.state,
            &self.stream_id,
            self.seat,
            &self.model_id,
            &review.file_path,
            &review.proposed_content,
            &title,
            review.old_content.clone(),
            &turn_id,
            crate::routes::agent::NativeReviewOperation {
                tool_call_id: review.tool_call_id.clone(),
                tool_name: review.tool_name.clone(),
                input: review.input.clone(),
                old_full: review.old_full.clone(),
            },
        ) {
            Ok(proposal) => proposal,
            Err(why) => return refuse(&why),
        };
        crate::state::set_agent_phase(
            &self.state,
            &agent_id,
            crate::state::ActivityPhase::AwaitingApproval {
                kind: crate::state::ApprovalKind::FileEdit,
                event_hash: proposal.event_hash.clone(),
            },
        );
        if matches!(
            operator_mode,
            Some(
                crate::agent_control::OperatingMode::AutoEdit
                    | crate::agent_control::OperatingMode::Auto
            )
        ) {
            crate::routes::agent::maybe_auto_accept_native_review(
                &self.state,
                &self.stream_id,
                &proposal,
            );
        }

        loop {
            if !acp_generation_is_current(&self.state, &self.stream_id, self.seat, self.generation)
            {
                return Ok(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Cancelled,
                ));
            }
            let cancelled = crate::state::agent_activity_snapshot(&self.state, &agent_id)
                .is_none_or(|activity| activity.turn_id != turn_id || activity.cancel_requested);
            if cancelled {
                if stop_asked(&self.state, &self.stream_id, self.seat, &turn_id) {
                    self.stopped_before(&turn_id, &request.tool_call);
                }
                return Ok(acp::RequestPermissionResponse::new(
                    acp::RequestPermissionOutcome::Cancelled,
                ));
            }
            if let Some(decision) =
                native_review_decision(&self.state, &self.stream_id, &proposal.event_hash)
            {
                return match decision {
                    Ok(final_content) => Ok(native_review_response(&request.options, &review, &final_content)),
                    Err(why) => refuse(&why),
                };
            }
            if cancellation.is_cancelled() {
                withdraw_pending_approval(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    &turn_id,
                    &proposal.event_hash,
                    "file_edit",
                );
                if stop_asked(&self.state, &self.stream_id, self.seat, &turn_id) {
                    self.stopped_before(&turn_id, &request.tool_call);
                }
                return Err(acp::Error::request_cancelled());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Everything up to a parked wait is synchronous, so `answer_off_loop`
    /// records the gate in arrival order on the dispatch loop and only the
    /// wait itself continues as a connection task.
    async fn request_permission(
        &self,
        request: acp::RequestPermissionRequest,
        cancellation: &RequestCancellation,
    ) -> acp::Result<acp::RequestPermissionResponse> {
        let boundary_turn = self.current_turn_id.lock().unwrap().clone();
        // A subagent's request never splits this agent's reply.
        if self
            .subagent_of(&request.tool_call.tool_call_id.0, request.tool_call.meta.as_ref())
            .is_none()
        {
            self.flush_text(&boundary_turn);
        }
        let (operator_mode, native_review_advertised, reasons, seat_tmp) = {
            let shared = self.shared.lock().unwrap();
            (shared.operator_mode, shared.snapshot.native_review, shared.review_reasons, shared.seat_tmp.clone())
        };
        if native_review_marker(&request).is_some() && !native_review_advertised {
            return Ok(acp::RequestPermissionResponse::new(permission_outcome(
                &request.options,
                ChildToolGateDecisionKind::Deny,
            )));
        }
        match native_review_request(&self.state, &self.stream_id, &request, &seat_tmp) {
            Ok(Some(review)) => {
                return self
                    .request_native_review_permission(&request, review, operator_mode, cancellation)
                    .await;
            }
            Ok(None) => {}
            // A runtime that advertises Bakery's extension but violates its
            // small input contract must fail closed. Falling through to an
            // ordinary allow path would let a malformed Edit/Write bypass
            // Review entirely. Claude hears why.
            Err(why) => {
                return Ok(review_refusal(&request.options, &request.tool_call.tool_call_id.0, &why, reasons));
            }
        }
        let kind = match request.tool_call.fields.kind {
            Some(acp::ToolKind::Read) => ChildToolKind::Read,
            Some(acp::ToolKind::Search) => ChildToolKind::Search,
            Some(acp::ToolKind::Think) => ChildToolKind::Think,
            Some(acp::ToolKind::Fetch) => ChildToolKind::Fetch,
            Some(acp::ToolKind::Edit) => ChildToolKind::Edit,
            Some(acp::ToolKind::Delete) => ChildToolKind::Delete,
            Some(acp::ToolKind::Move) => ChildToolKind::Move,
            Some(acp::ToolKind::Execute) => ChildToolKind::Execute,
            Some(acp::ToolKind::SwitchMode) => ChildToolKind::SwitchMode,
            _ => ChildToolKind::Other,
        };
        // The Baker's own tools: `api_tools::call` grades and gates each call
        // where it runs, so the runtime may make it, and a call that needs the
        // operator asks there, once.
        if kind == ChildToolKind::Other
            && is_bakery_tool_call(request.tool_call.fields.title.as_deref().unwrap_or(""))
            && crate::mcp::mcp_scope_for_stream(&self.state, &self.stream_id)
                == Some(crate::mcp::McpScope::BakerOps)
        {
            return Ok(acp::RequestPermissionResponse::new(permission_outcome(
                &request.options,
                ChildToolGateDecisionKind::AllowOnce,
            )));
        }
        // Your tool policy outranks every automatic mode: a kind set to
        // "never" is refused — and recorded, so the refusal is visible — even
        // in Auto or on a trusted socket.
        if tool_policy_denies(&self.state, kind) {
            let turn_id = self.current_turn_id.lock().unwrap().clone();
            let req = ChildToolRequest {
                plug: self.model_id.clone(),
                kind,
                title: request.tool_call.fields.title.clone().unwrap_or_default(),
                diffs: request_diffs(&request),
            };
            admit_child_tool_call_detailed(
                &self.state,
                &self.stream_id,
                self.seat,
                &turn_id,
                &req,
                false,
            );
            return Ok(acp::RequestPermissionResponse::new(permission_outcome(
                &request.options,
                ChildToolGateDecisionKind::Deny,
            )));
        }
        let automatic = match operator_mode {
            Some(crate::agent_control::OperatingMode::Plan) => Some(matches!(
                kind,
                ChildToolKind::Read
                    | ChildToolKind::Search
                    | ChildToolKind::Think
                    | ChildToolKind::Fetch
            )),
            Some(crate::agent_control::OperatingMode::AutoEdit) => Some(matches!(
                kind,
                ChildToolKind::Read
                    | ChildToolKind::Search
                    | ChildToolKind::Think
                    | ChildToolKind::Edit
                    | ChildToolKind::Delete
                    | ChildToolKind::Move
            )),
            Some(crate::agent_control::OperatingMode::Auto) => Some(true),
            Some(crate::agent_control::OperatingMode::Manual) => None,
            None if self.trusted => Some(true),
            None => None,
        };
        if let Some(allow) = automatic {
            let decision = if allow {
                ChildToolGateDecisionKind::AllowOnce
            } else {
                ChildToolGateDecisionKind::Deny
            };
            return Ok(acp::RequestPermissionResponse::new(permission_outcome(
                &request.options,
                decision,
            )));
        }
        let title = request.tool_call.fields.title.clone().unwrap_or_default();
        let req = ChildToolRequest {
            plug: self.model_id.clone(),
            kind,
            title,
            diffs: request_diffs(&request),
        };
        // A workspace edit that shows its exact changes can be approved in
        // Manual like any proposal — the operator sees what they allow.
        let shows_its_changes = kind.is_workspace_mutation() && !req.diffs.is_empty();
        let turn_id = self.current_turn_id.lock().unwrap().clone();
        let admission = admit_child_tool_call_detailed(
            &self.state,
            &self.stream_id,
            self.seat,
            &turn_id,
            &req,
            (self.trusted && operator_mode.is_some()) || shows_its_changes,
        );
        let outcome = match admission {
            ChildToolAdmission::Allow => {
                permission_outcome(&request.options, ChildToolGateDecisionKind::AllowOnce)
            }
            ChildToolAdmission::Deny => {
                permission_outcome(&request.options, ChildToolGateDecisionKind::Deny)
            }
            ChildToolAdmission::Pending(pending_hash) => {
                // A direct `Model::chat` fixture has no live turn to resume;
                // retain the historic deny-closed behavior there.  A real
                // Bakery turn parks on the adapter's request until the exact
                // Heart decision arrives or Stop/reassignment invalidates it.
                let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
                let live_turn = !turn_id.is_empty()
                    && crate::state::agent_activity_snapshot(&self.state, &agent_id)
                        .is_some_and(|activity| activity.turn_id == turn_id);
                if !live_turn {
                    acp::RequestPermissionOutcome::Cancelled
                } else {
                    loop {
                        if !acp_generation_is_current(
                            &self.state,
                            &self.stream_id,
                            self.seat,
                            self.generation,
                        ) {
                            break acp::RequestPermissionOutcome::Cancelled;
                        }
                        let cancelled =
                            crate::state::agent_activity_snapshot(&self.state, &agent_id)
                                .is_none_or(|activity| {
                                    activity.turn_id != turn_id || activity.cancel_requested
                                });
                        if cancelled {
                            if stop_asked(&self.state, &self.stream_id, self.seat, &turn_id) {
                                self.stopped_before(&turn_id, &request.tool_call);
                            }
                            break acp::RequestPermissionOutcome::Cancelled;
                        }
                        if let Some(decision) =
                            child_tool_gate_decision(&self.state, &self.stream_id, &pending_hash)
                        {
                            if decision == ChildToolGateDecisionKind::AllowOnce {
                                // `decide_child_tool_gate` retains its public
                                // next-call allowance semantics.  This parked
                                // callback consumes that allowance itself so
                                // it cannot accidentally authorize a second
                                // native action.
                                self.state
                                    .child_tool_allow_once
                                    .lock()
                                    .unwrap()
                                    .remove(&(self.stream_id.clone(), kind));
                            }
                            break permission_outcome(&request.options, decision);
                        }
                        if cancellation.is_cancelled() {
                            withdraw_pending_approval(
                                &self.state,
                                &self.stream_id,
                                self.seat,
                                &turn_id,
                                &pending_hash,
                                "child_tool",
                            );
                            if stop_asked(&self.state, &self.stream_id, self.seat, &turn_id) {
                                self.stopped_before(&turn_id, &request.tool_call);
                            }
                            return Err(acp::Error::request_cancelled());
                        }
                        tokio::time::sleep(Duration::from_millis(20)).await;
                    }
                }
            }
        };
        Ok(acp::RequestPermissionResponse::new(outcome))
    }

    /// Runs on the dispatch loop, in arrival order, so text and tool calls
    /// land in Heart exactly as interleaved as the runtime sent them.
    fn session_notification(&self, notification: acp::SessionNotification) -> acp::Result<()> {
        if !acp_generation_is_current(&self.state, &self.stream_id, self.seat, self.generation) {
            return Ok(());
        }
        // `SessionUpdate` is `#[non_exhaustive]` upstream — every variant
        // this build doesn't act on falls through the wildcard arm, never a
        // silent `if let` miss (07d-A3B review #2: only text chunks used to
        // be recorded; native reads/commands/edits/locations/outcomes were
        // discarded entirely).
        match notification.update {
            // A subagent's words belong to it, not to this agent's reply.
            acp::SessionUpdate::AgentMessageChunk(chunk) if subagent_parent(chunk.meta.as_ref()).is_some() => {
                if let (Some(parent), acp::ContentBlock::Text(text)) =
                    (subagent_parent(chunk.meta.as_ref()), chunk.content)
                {
                    let mut buffers = self.subagent_text.lock().unwrap();
                    buffer_subagent_text(buffers.entry(parent).or_default(), &text.text);
                }
            }
            // The agent's own thinking (W1-T); a subagent's isn't its.
            acp::SessionUpdate::AgentThoughtChunk(chunk) if subagent_parent(chunk.meta.as_ref()).is_none() => {
                let text = match chunk.content {
                    acp::ContentBlock::Text(text) => text.text,
                    _ => String::new(),
                };
                self.thinking.lock().unwrap().thought(&text);
                let turn_id = self.current_turn_id.lock().unwrap().clone();
                crate::state::note_thought_for_turn(
                    &self.state,
                    &crate::node::agent_id(&self.stream_id, self.seat),
                    &turn_id,
                    text.chars().count(),
                );
            }
            acp::SessionUpdate::AgentMessageChunk(chunk) => {
                self.settle_thinking();
                if let acp::ContentBlock::Text(text) = chunk.content {
                    // A reply of any length is kept whole: nothing cuts a
                    // turn off for what it says.
                    let mut t = self.text.lock().unwrap();
                    t.push_str(&text.text);
                    if let Some(delta_tx) = self.current_delta_tx.lock().unwrap().as_ref() {
                        let _ = delta_tx.send(text.text.clone());
                    }
                    drop(t);
                    // The live text is painted here, in order with the tool
                    // calls around it. Painted from the prompt loop's 20ms
                    // drain instead, a chunk written just before a tool call
                    // landed after it — the running tool's line turned back
                    // into text the transcript had already recorded.
                    let turn_id = self.current_turn_id.lock().unwrap().clone();
                    crate::state::append_streaming_text_for_turn(
                        &self.state,
                        &crate::node::agent_id(&self.stream_id, self.seat),
                        &turn_id,
                        &text.text,
                    );
                }
            }
            acp::SessionUpdate::ToolCall(tool_call) => {
                let turn_id = self.current_turn_id.lock().unwrap().clone();
                // A subagent's step never splits this agent's reply; the
                // subagent's own words up to it land first, under its call.
                match subagent_parent(tool_call.meta.as_ref()) {
                    Some(parent) => {
                        self.subagent_calls
                            .lock()
                            .unwrap()
                            .insert(tool_call.tool_call_id.0.to_string(), parent.clone());
                        self.flush_subagent_text(&turn_id, &parent);
                    }
                    None => {
                        self.settle_thinking();
                        self.flush_text(&turn_id);
                    }
                }
                record_acp_tool_event(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    &self.model_id,
                    &turn_id,
                    "tool_call",
                    &tool_call,
                );
                self.tool_titles
                    .lock()
                    .unwrap()
                    .insert(tool_call.tool_call_id.0.to_string(), tool_call.title.clone());
                // Live activity: reflect the native tool call as RunningTool,
                // but only while this is still the agent's current, un-cancelled
                // turn (mirrors the child-tool gate's `still_this_turn` guard).
                // Never over an open approval: the adapter can re-announce a
                // call after asking permission for it, and "running" would
                // then hide the one thing the operator must act on.
                let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
                let still_this_turn = crate::state::agent_activity_snapshot(&self.state, &agent_id)
                    .is_some_and(|a| {
                        a.turn_id == turn_id
                            && !a.cancel_requested
                            && !matches!(
                                a.phase,
                                crate::state::ActivityPhase::AwaitingApproval { .. }
                            )
                    });
                if still_this_turn {
                    crate::state::set_agent_phase(
                        &self.state,
                        &agent_id,
                        native_tool_running_phase(
                            &tool_call.title,
                            &tool_call.tool_call_id.0,
                            chrono::Utc::now().to_rfc3339(),
                            tool_preview(tool_call.raw_input.as_ref()),
                        ),
                    );
                }
                self.start_command(&turn_id, &tool_call);
            }
            acp::SessionUpdate::ToolCallUpdate(mut update) => {
                let turn_id = self.current_turn_id.lock().unwrap().clone();
                // A subagent's launching call finishing: its last words land
                // before its result.
                let call_id = update.tool_call_id.0.to_string();
                if self.subagent_of(&call_id, update.meta.as_ref()).is_none()
                    && matches!(
                        update.fields.status,
                        Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                    )
                {
                    // A step ended: thinking after it starts from here.
                    self.settle_thinking();
                    self.flush_subagent_text(&turn_id, &call_id);
                }
                if self.follow_command(&turn_id, &mut update) {
                    return Ok(());
                }
                record_acp_tool_event(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    &self.model_id,
                    &turn_id,
                    "tool_call_update",
                    &update,
                );
                if let Some(
                    status @ (acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed),
                ) = update.fields.status
                {
                    let title = self.tool_titles.lock().unwrap().remove(&call_id);
                    // A call that fails as Stop tears it down had started:
                    // what it got done is above, and no crash ended it.
                    if status == acp::ToolCallStatus::Failed
                        && title.is_some()
                        && stop_asked(&self.state, &self.stream_id, self.seat, &turn_id)
                    {
                        let detail = serde_json::json!({ "toolCallId": call_id });
                        record_acp_tool_event(&self.state, &self.stream_id, self.seat, &self.model_id, &turn_id, STOPPED_AFTER, &detail);
                    }
                    crate::state::end_native_tool(
                        &self.state,
                        &crate::node::agent_id(&self.stream_id, self.seat),
                        &turn_id,
                        &call_id,
                        update.fields.title.clone().or(title),
                        status == acp::ToolCallStatus::Completed,
                    );
                }
            }
            acp::SessionUpdate::CurrentModeUpdate(update) => {
                self.shared.lock().unwrap().snapshot.current_mode =
                    Some(update.current_mode_id.0.to_string());
            }
            acp::SessionUpdate::ConfigOptionUpdate(update) => {
                self.shared
                    .lock()
                    .unwrap()
                    .replace_config_options(update.config_options);
            }
            acp::SessionUpdate::AvailableCommandsUpdate(update) => {
                self.shared.lock().unwrap().snapshot.commands = update
                    .available_commands
                    .iter()
                    .map(project_command)
                    .collect();
            }
            acp::SessionUpdate::UsageUpdate(update) => {
                let mut shared = self.shared.lock().unwrap();
                shared.snapshot.usage = Some(AcpUsage {
                    used: update.used,
                    size: update.size,
                });
                // A reading without a cost (a rate-limit beat) says nothing
                // about spend; the last priced reading stands.
                if update.cost.is_some() {
                    shared.cost = update.cost;
                }
            }
            acp::SessionUpdate::CompactionSummaryChunk(chunk) => {
                if let acp::ContentBlock::Text(text) = &chunk.content {
                    let mut open = self.compactions.lock().unwrap();
                    open.entry(chunk.compaction_id.0.to_string()).or_default().push_str(&text.text);
                }
            }
            acp::SessionUpdate::CompactionUpdate(update) => self.record_compaction(&notification.session_id, update),
            _ => {}
        }
        Ok(())
    }

    /// A native compaction once it ends, as the thread shows the Bakery's
    /// own (`agent.context.compacted`, `native: true`, with its summary).
    /// Each once: a resumed session replays the ones it holds. The update
    /// carries no size; the session's own record does, read once the prompt
    /// ends ([`fill_compaction_sizes`]), so the session it came in is kept.
    fn record_compaction(&self, session_id: &acp::SessionId, update: acp::CompactionUpdate) {
        let id = update.compaction_id.0.to_string();
        let status = match &update.status {
            acp::CompactionStatus::InProgress => return,
            acp::CompactionStatus::Completed => "completed".to_string(),
            acp::CompactionStatus::Failed => "failed".to_string(),
            acp::CompactionStatus::Cancelled => "cancelled".to_string(),
            acp::CompactionStatus::Other(other) => other.clone(),
            _ => "unknown".to_string(),
        };
        let streamed = self.compactions.lock().unwrap().remove(&id).unwrap_or_default();
        // Its summary in full, else what streamed in (a summary already
        // sent as chunks isn't sent again).
        let said = serde_json::to_value(&update).unwrap_or_default();
        let summary = said["summary"]
            .as_array()
            .map(|blocks| blocks.iter().filter_map(|block| block["text"].as_str()).collect::<String>())
            .unwrap_or(streamed);
        let error = said["error"].as_str().map(str::to_string);
        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
        let recorded = self.state.heart.lock().unwrap().events_of_family(&self.stream_id, "agent.context.compacted").is_ok_and(|events| {
            events.iter().any(|e| {
                serde_json::from_str::<serde_json::Value>(&e.payload)
                    .is_ok_and(|p| p["native"] == true && p["compaction_id"] == id.as_str() && p["agent_id"] == agent_id.as_str())
            })
        });
        if recorded {
            return;
        }
        let payload = serde_json::json!({
            "agent_id": agent_id,
            "turn_id": self.current_turn_id.lock().unwrap().clone(),
            "model_id": self.model_id,
            "native": true,
            "compaction_id": id,
            "session_id": session_id.0.as_ref(),
            "status": status,
            "summary": (!summary.trim().is_empty()).then_some(summary),
            "error": error,
        });
        let actor = format!("agent:{}", self.seat.as_str());
        let _ = crate::state::append(&self.state, &self.stream_id, &actor, "agent.context.compacted", payload.to_string(), vec![]);
    }

    fn auth_status_update(&self, params: &serde_json::Value) {
        if !acp_generation_is_current(&self.state, &self.stream_id, self.seat, self.generation) {
            return;
        }
        if let Some(status) = project_auth_status(params) {
            self.shared.lock().unwrap().snapshot.auth_status = Some(status);
        }
    }
}

/// A native compaction's size, beside it (its causality): the context's size
/// in tokens when it was compacted (`tokens`), as the session's own record
/// has it, and which entry there (`boundary`); `tokens` null when it holds
/// none.
pub(crate) const COMPACTION_SIZE: &str = "agent.context.compaction_size";

/// How far apart a compaction and its entry in the session's record may be.
const COMPACTION_NEAR_SECS: i64 = 300;

/// Gives each completed native compaction of `session_id` on this agent
/// that has no size yet the one the Bakery's copy of the session records
/// (`resume::kept_compactions`): the entry by its id, else the one nearest
/// in time no other compaction took. The update itself carries no size, and
/// the runtime may write its record only as the prompt ends; with `settle`
/// (the next prompt starting), one still without a size gets none for good.
fn fill_compaction_sizes(state: &AppState, stream_id: &str, seat: Seat, session_id: &str, settle: bool) {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let fact = |e: &bakery_core::Event| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap_or_default();
    let (compacted, sized) = {
        let heart = state.heart.lock().unwrap();
        let of = |family: &str| heart.events_of_family(stream_id, family).unwrap_or_default();
        (of("agent.context.compacted"), of(COMPACTION_SIZE).iter().map(fact).collect::<Vec<_>>())
    };
    let open: Vec<(bakery_core::Event, serde_json::Value)> = compacted
        .into_iter()
        .map(|e| {
            let p = fact(&e);
            (e, p)
        })
        .filter(|(_, p)| p["native"] == true && p["status"] == "completed" && p["agent_id"] == agent_id.as_str() && p["session_id"] == session_id)
        .filter(|(_, p)| !sized.iter().any(|s| s["compaction_id"] == p["compaction_id"]))
        .collect();
    if open.is_empty() {
        return;
    }
    let mut kept = crate::resume::kept_compactions(state, &agent_id, session_id);
    kept.retain(|k| !sized.iter().any(|s| s["boundary"] == k.uuid.as_str()));
    for (event, p) in open {
        let id = p["compaction_id"].as_str().unwrap_or_default();
        let at = chrono::DateTime::parse_from_rfc3339(&event.ts).ok().map(|t| t.with_timezone(&chrono::Utc));
        let apart = |k: &crate::resume::KeptCompaction| at.map(|at| (k.at - at).num_milliseconds().abs());
        let nearest = || {
            (0..kept.len())
                .filter(|&i| apart(&kept[i]).is_some_and(|ms| ms <= COMPACTION_NEAR_SECS * 1000))
                .min_by_key(|&i| apart(&kept[i]))
        };
        let found = kept.iter().position(|k| k.uuid == id).or_else(nearest).map(|i| kept.remove(i));
        if found.is_none() && !settle {
            continue;
        }
        let size = serde_json::json!({
            "agent_id": agent_id,
            "turn_id": p["turn_id"],
            "compaction_id": id,
            "tokens": found.as_ref().and_then(|k| k.tokens),
            "boundary": found.as_ref().map(|k| k.uuid.as_str()),
        });
        let actor = format!("agent:{}", seat.as_str());
        let _ = crate::state::append(state, stream_id, &actor, COMPACTION_SIZE, size.to_string(), vec![event.event_hash]);
    }
}

/// The adapter's `_auth/status_update` extension notification, kept as the
/// raw JSON it arrived as: `project_auth_status` picks out the safe fields.
#[derive(Debug, Clone, Serialize, Deserialize, agent_client_protocol::JsonRpcNotification)]
#[notification(method = "_auth/status_update")]
struct AuthStatusUpdate(serde_json::Value);

/// The runtime withdrew a request a human was deciding (`$/cancel_request`).
/// Close that exact card the way Stop does, so nobody can still answer a
/// question no turn is waiting for. Shares the decision lock with every
/// decision route, and leaves an already-decided card alone.
pub(crate) fn withdraw_pending_approval(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    event_hash: &str,
    kind: &str,
) {
    let reason = "the native runtime withdrew this request";
    withdraw_pending_approval_because(state, stream_id, seat, turn_id, event_hash, kind, reason);
}

/// [`withdraw_pending_approval`], saying why.
pub(crate) fn withdraw_pending_approval_because(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    turn_id: &str,
    event_hash: &str,
    kind: &str,
    reason: &str,
) {
    let _decision = state.edit_decision_lock.lock().unwrap();
    let closed = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .map(|events| {
            events.iter().any(|event| {
                crate::routes::agent::closes_pending_operation(&event.family)
                    && event.causality.iter().any(|hash| hash == event_hash)
            })
        })
        .unwrap_or(true);
    if closed {
        return;
    }
    let agent_id = crate::node::agent_id(stream_id, seat);
    let payload = serde_json::json!({
        "decided_event": event_hash,
        "status": "cancelled",
        "reason": reason,
        "kind": kind,
        "agent_id": agent_id,
        "turn_id": turn_id,
    })
    .to_string();
    if crate::state::append(
        state,
        stream_id,
        &format!("agent:{}", seat.as_str()),
        "agent.approval.cancelled",
        payload,
        vec![event_hash.to_string()],
    )
    .is_ok()
    {
        crate::state::resolve_awaiting_approval(
            state,
            &agent_id,
            event_hash,
            crate::state::ActivityPhase::ContactingModel,
        );
    }
}

/// Answers a runtime request whose work may park on a human. In 2.x every
/// handler runs on the connection's one dispatch loop, so the work runs here
/// only up to its first wait — what it records lands in arrival order with
/// the updates around it — and the wait continues as a connection task, so
/// streaming never stalls behind a decision. Every path answers: a request
/// left unanswered would hang the runtime.
async fn answer_off_loop<T: JsonRpcResponse>(
    work: impl std::future::Future<Output = acp::Result<T>> + Send + 'static,
    responder: Responder<T>,
    cx: &ConnectionTo<Agent>,
) -> acp::Result<()> {
    let mut work = Box::pin(work);
    if let Poll::Ready(result) = futures::poll!(work.as_mut()) {
        return responder.respond_with_result(result);
    }
    cx.spawn(async move { responder.respond_with_result(work.await) })
}

/// Serves one ACP connection to the adapter until its output ends, handing
/// the session loop the handle every request goes out on.
async fn serve_gate_client(
    client: Arc<GateClient>,
    transport: impl agent_client_protocol::ConnectTo<Client> + 'static,
    connection_tx: tokio::sync::oneshot::Sender<ConnectionTo<Agent>>,
) -> acp::Result<()> {
    let permission_client = client.clone();
    let elicitation_client = client.clone();
    let update_client = client.clone();
    Client
        .builder()
        .name("bakery-node")
        .on_receive_request(
            async move |request: acp::RequestPermissionRequest,
                        responder: Responder<acp::RequestPermissionResponse>,
                        cx: ConnectionTo<Agent>| {
                let client = permission_client.clone();
                let cancellation = responder.cancellation();
                answer_off_loop(
                    async move { client.request_permission(request, &cancellation).await },
                    responder,
                    &cx,
                )
                .await
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |request: acp::CreateElicitationRequest,
                        responder: Responder<acp::CreateElicitationResponse>,
                        cx: ConnectionTo<Agent>| {
                let client = elicitation_client.clone();
                let cancellation = responder.cancellation();
                answer_off_loop(
                    async move {
                        // `crate::ask` reads the form as the JSON it always
                        // parsed, and its reply is that JSON too.
                        let params = serde_json::to_value(&request)
                            .map_err(acp::Error::into_internal_error)?;
                        let reply = client.elicit(&params, &cancellation).await?;
                        serde_json::from_value(reply).map_err(acp::Error::into_internal_error)
                    },
                    responder,
                    &cx,
                )
                .await
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx: ConnectionTo<Agent>| {
                update_client.session_notification(notification)
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .on_receive_notification(
            async move |update: AuthStatusUpdate, _cx: ConnectionTo<Agent>| {
                client.auth_status_update(&update.0);
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(transport, async move |connection: ConnectionTo<Agent>| {
            let _ = connection_tx.send(connection.clone());
            // Stay connected until the adapter's output ends: the session
            // loop sends every request itself, over the handle sent above.
            connection.incoming_closed().await;
            Ok(())
        })
        .await
}

fn select_agent_auth_method(
    init: &acp::InitializeResponse,
    env_var_methods: &[String],
) -> Result<Option<acp::AuthMethodId>, String> {
    // Prefer the adapter's declared default. This is provider-neutral and,
    // critically, does not re-derive account or billing precedence from
    // names such as `cached_token`/`grok.com` in the common ACP client.
    if let Some(default_id) = init
        .meta
        .as_ref()
        .and_then(|meta| meta.get("defaultAuthMethodId"))
        .and_then(serde_json::Value::as_str)
    {
        let method = init
            .auth_methods
            .iter()
            .find(|method| method.id().0.as_ref() == default_id)
            .ok_or_else(|| {
                format!(
                    "Setup needed: ACP adapter declared unavailable default auth method '{default_id}'"
                )
            })?;
        return match auth_method_kind(method, env_var_methods) {
            "agent" => Ok(Some(method.id().clone())),
            "terminal" => Ok(None),
            "environment" => Err(format!(
                "Setup needed: ACP adapter selected environment credential method '{default_id}', but Bakery does not pass credentials into native agent sessions"
            )),
            _ => Err(format!(
                "Setup needed: ACP adapter selected unsupported auth method '{default_id}'"
            )),
        };
    }

    // Terminal methods are launch contracts, not ids to pass back through
    // `authenticate`. A previously signed-in native adapter proceeds to
    // session/new; an unauthenticated one returns auth_required and Provider
    // Accounts launches the server-registered terminal flow.
    if init
        .auth_methods
        .iter()
        .any(|method| matches!(method, acp::AuthMethod::Terminal(_)))
    {
        return Ok(None);
    }
    let agent_methods: Vec<&acp::AuthMethod> = init
        .auth_methods
        .iter()
        .filter(|method| auth_method_kind(method, env_var_methods) == "agent")
        .collect();
    match agent_methods.as_slice() {
        [] if init.auth_methods.is_empty() => return Ok(None),
        [method] => return Ok(Some(method.id().clone())),
        [] => {}
        _ => {
            return Err(
                "Setup needed: ACP adapter advertised multiple agent-managed authentication methods without a defaultAuthMethodId; sign in through Provider Accounts or configure the adapter to declare one"
                    .to_string(),
            )
        }
    }
    let advertised: Vec<&str> = init
        .auth_methods
        .iter()
        .map(|m| m.id().0.as_ref())
        .collect();
    Err(format!(
        "Setup needed: ACP adapter requires client-supplied environment credentials (advertised: {}); native sessions never receive Bakery credentials",
        if advertised.is_empty() { "none".to_string() } else { advertised.join(", ") }
    ))
}

const ACP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a native session may take to resume: a long one loads its whole
/// history first.
const ACP_RESUME_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a caller waits for another caller's start (or the old session's
/// reaping) on the same seat. A freshly restarted node gets overlapping
/// requests for one session — a reconnecting pane asks for its capabilities
/// more than once — and each should get that session, not "retry shortly".
const ACP_START_WAIT: Duration = Duration::from_secs(90);
/// How long a setting change may take before Bakery stops waiting for it.
const ACP_CONFIG_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(5)
} else {
    ACP_HANDSHAKE_TIMEOUT
};

/// How a timed-out setting change's error begins; the config route answers
/// 504 for it, and the composer then offers a runtime restart.
pub(crate) const CONFIG_UNANSWERED: &str = "The runtime didn't answer";

fn config_unanswered() -> String {
    format!(
        "{CONFIG_UNANSWERED} within {}s, so this setting is not confirmed. The picker keeps the last confirmed value, and Bakery sends that value again before the next turn. If the network is up and this keeps happening, restart the runtime.",
        ACP_CONFIG_TIMEOUT.as_secs()
    )
}

async fn acp_timeout<T>(
    name: &'static str,
    timeout: Duration,
    future: impl std::future::Future<Output = acp::Result<T>>,
) -> Result<T, String> {
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| format!("ACP {name} timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("ACP {name} failed: {e}"))
}

// ─── History replay on (re)spawn ─────────────────────────────────────────────

/// Bounds on the catch-up context prepended to a freshly (re)spawned
/// session's first prompt — the CLI's own conversational state died with
/// its process (07d-A3B review #1: "a restarted child creates a fresh ACP
/// session"); Heart is what makes it not actually amnesiac.
const MAX_CATCHUP_TURNS: usize = 10;
const MAX_CATCHUP_CHARS: usize = 20_000;

/// Shared persona coverage for every seat, not only Baker/task sockets.
fn acp_persona_preamble(state: &AppState, stream_id: &str, seat: Seat) -> String {
    crate::agent_loop::persona_for_agent(state, stream_id, seat)
}

/// Render this agent's prior turns as a bounded transcript to prepend to a
/// fresh session's very first prompt. A turn that did not finish counts too:
/// a failed prompt kills the process (see `session_main`), so the session
/// after a network error is exactly the one that must still know what that
/// turn was asked, wrote and ran. `agent.turn.done` carries an ACP turn's
/// FULL reply in `summary`, so finished turns replay in full, newest kept
/// first. Empty when there is nothing to replay (a brand new agent_id).
fn acp_catchup_context(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    current_turn_id: &str,
) -> String {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let turns = crate::agent_loop::agent_prior_turns(state, stream_id, &agent_id, current_turn_id);
    let body = crate::agent_loop::render_prior_turns(&turns, MAX_CATCHUP_TURNS, MAX_CATCHUP_CHARS);
    if body.is_empty() {
        return String::new();
    }
    format!(
        "[Bakery: this CLI process just (re)started; the conversation itself continues from \
         Heart, not the process. Prior turns on this agent, including any that did not \
         finish:]\n\n{body}[End of prior turns — continue from here:]\n\n"
    )
}

fn raw_config_value(
    shared: &AcpSessionShared,
    config_id: &str,
    value: &AcpConfigValue,
) -> Result<(acp::SessionConfigOptionValue, Option<String>), String> {
    let option = shared
        .raw_config_options
        .iter()
        .find(|option| option.id.0.as_ref() == config_id)
        .ok_or_else(|| {
            format!("ACP session does not advertise configuration option '{config_id}'")
        })?;
    let category = option.category.as_ref().map(project_config_category);
    match (&option.kind, value) {
        (acp::SessionConfigKind::Select(select), AcpConfigValue::Select(requested)) => {
            let present = match &select.options {
                acp::SessionConfigSelectOptions::Ungrouped(options) => options
                    .iter()
                    .any(|option| option.value.0.as_ref() == requested),
                acp::SessionConfigSelectOptions::Grouped(groups) => groups.iter().any(|group| {
                    group
                        .options
                        .iter()
                        .any(|option| option.value.0.as_ref() == requested)
                }),
                _ => false,
            };
            if !present {
                return Err(format!(
                    "'{requested}' is not currently available for ACP configuration option '{config_id}'"
                ));
            }
            Ok((
                acp::SessionConfigOptionValue::value_id(acp::SessionConfigValueId::new(
                    requested.clone(),
                )),
                category,
            ))
        }
        (acp::SessionConfigKind::Boolean(_), AcpConfigValue::Boolean(requested)) => {
            Ok((acp::SessionConfigOptionValue::boolean(*requested), category))
        }
        (acp::SessionConfigKind::Select(_), AcpConfigValue::Boolean(_)) => Err(format!(
            "ACP configuration option '{config_id}' requires a selectable value"
        )),
        (acp::SessionConfigKind::Boolean(_), AcpConfigValue::Select(_)) => Err(format!(
            "ACP configuration option '{config_id}' requires a boolean value"
        )),
        _ => Err(format!(
            "ACP configuration option '{config_id}' has a kind this Bakery build cannot set"
        )),
    }
}

/// Project the latest acknowledged preference for each native configuration
/// id: a pick names its option, a choice made with no session (thread_create,
/// `selection`) names its kind, which `option_of` turns into this session's
/// option. These are durable *desired* values for this exact Bakery agent and
/// runtime identity, not a live capability cache: a newly created ACP
/// generation still validates every item against its own setup response
/// before using it.
fn durable_native_config_snapshot(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    runtime_id: &str,
    option_of: impl Fn(&str) -> Option<String>,
) -> Option<NativeConfigSnapshot> {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let events = state
        .heart
        .lock()
        .unwrap()
        .read_stream(stream_id)
        .unwrap_or_default();
    let mut values = BTreeMap::<String, NativeConfigValue>::new();
    let mut source_generation = None;
    for event in events.iter().rev() {
        if event.family != "agent.native_config.acknowledged" {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
            continue;
        };
        if payload.get("agent_id").and_then(serde_json::Value::as_str) != Some(agent_id.as_str())
            || payload
                .get("account_id")
                .and_then(serde_json::Value::as_str)
                != Some(runtime_id)
        {
            continue;
        }
        let config_id = match payload.get("config_id").and_then(serde_json::Value::as_str) {
            Some(id) => id.to_string(),
            None => match payload.get("category").and_then(serde_json::Value::as_str).and_then(&option_of) {
                Some(id) => id,
                None => continue,
            },
        };
        let value = match payload.get("effective") {
            Some(serde_json::Value::String(value)) => NativeConfigValue::Select(value.clone()),
            Some(serde_json::Value::Bool(value)) => NativeConfigValue::Boolean(*value),
            _ => continue,
        };
        source_generation.get_or_insert_with(|| {
            payload
                .get("generation")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_default()
        });
        values.entry(config_id).or_insert(value);
    }
    (!values.is_empty()).then(|| NativeConfigSnapshot {
        runtime_id: runtime_id.to_string(),
        generation: source_generation.unwrap_or_default(),
        values: values
            .into_iter()
            .map(|(id, value)| NativeConfigSelection { id, value })
            .collect(),
    })
}

/// Best-effort recovery of acknowledged controls after a native child is
/// recreated. Unsupported/stale choices are deliberately skipped rather than
/// treated as acknowledged; the fresh runtime snapshot remains the source of
/// truth visible to the composer. Transport setup itself is not failed merely
/// because a subscription changed its available choices while Bakery was down.
async fn restore_durable_native_config(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    shared: &Arc<Mutex<AcpSessionShared>>,
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    runtime_id: &str,
) {
    let options: Vec<(String, Option<String>)> = {
        let guard = shared.lock().unwrap();
        guard
            .raw_config_options
            .iter()
            .map(|option| (option.id.0.to_string(), option.category.as_ref().map(project_config_category)))
            .collect()
    };
    let option_of = |category: &str| options.iter().find(|(_, c)| c.as_deref() == Some(category)).map(|(id, _)| id.clone());
    let Some(snapshot) = durable_native_config_snapshot(state, stream_id, seat, runtime_id, option_of) else {
        return;
    };
    for stage in [Some("model"), Some("thought_level"), None] {
        for selection in &snapshot.values {
            let validated = {
                let guard = shared.lock().unwrap();
                let category = guard
                    .raw_config_options
                    .iter()
                    .find(|option| option.id.0.as_ref() == selection.id)
                    .and_then(|option| option.category.as_ref().map(project_config_category));
                let selected = match stage {
                    Some(stage) => category.as_deref() == Some(stage),
                    None => !matches!(category.as_deref(), Some("model" | "thought_level")),
                };
                selected.then(|| {
                    raw_config_value(
                        &guard,
                        &selection.id,
                        &AcpPipeModel::native_value(&selection.value),
                    )
                })
            };
            let Some(Ok((wire_value, category))) = validated else {
                continue;
            };
            let request = connection
                .send_request(acp::SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    selection.id.clone(),
                    wire_value,
                ))
                .block_task();
            let response = match tokio::time::timeout(ACP_CONFIG_TIMEOUT, request).await {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => continue,
                Err(_) => {
                    // Like a picker change that timed out, it may still land.
                    let mut guard = shared.lock().unwrap();
                    guard.unconfirmed.insert(selection.id.clone());
                    continue;
                }
            };
            let mut guard = shared.lock().unwrap();
            guard.unconfirmed.remove(&selection.id);
            if category.as_deref() == Some("model") {
                guard.snapshot.requested_model = match &selection.value {
                    NativeConfigValue::Select(value) => Some(value.clone()),
                    NativeConfigValue::Boolean(_) => None,
                };
            }
            guard.replace_config_options(response.config_options);
        }
    }
}

async fn handle_acp_control(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    generation: u64,
    shared: &Arc<Mutex<AcpSessionShared>>,
    control: AcpControlMsg,
) {
    match control {
        AcpControlMsg::Cancel {
            generation: requested_generation,
            reply_tx,
        } => {
            if requested_generation != generation {
                let _ = reply_tx.send(Err(
                    "ACP session generation changed before cancellation".to_string()
                ));
                return;
            }
            // A notification is only queued for the writer, so there is no
            // reply to wait for; the prompt itself settles with `cancelled`.
            let result = connection
                .send_notification(acp::CancelNotification::new(session_id.clone()))
                .map_err(|e| format!("ACP session/cancel failed: {e}"));
            let _ = reply_tx.send(result);
        }
        AcpControlMsg::SetConfig {
            generation: requested_generation,
            config_id,
            value,
            reply_tx,
        } => {
            if requested_generation != generation {
                let _ = reply_tx.send(Err(
                    "ACP session generation changed before configuration was applied".to_string(),
                ));
                return;
            }
            let validated = {
                let guard = shared.lock().unwrap();
                raw_config_value(&guard, &config_id, &value)
            };
            let (wire_value, category) = match validated {
                Ok(value) => value,
                Err(error) => {
                    let _ = reply_tx.send(Err(error));
                    return;
                }
            };
            let request = connection
                .send_request(acp::SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    config_id.clone(),
                    wire_value,
                ))
                .block_task();
            let response = tokio::time::timeout(ACP_CONFIG_TIMEOUT, request).await;
            let mut guard = shared.lock().unwrap();
            let result = match response {
                Err(_) => {
                    guard.unconfirmed.insert(config_id);
                    Err(config_unanswered())
                }
                Ok(Err(e)) => Err(format!("ACP session/set_config_option failed: {e}")),
                Ok(Ok(response)) => {
                    guard.unconfirmed.remove(&config_id);
                    if category.as_deref() == Some("model") {
                        guard.snapshot.requested_model = match &value {
                            AcpConfigValue::Select(value) => Some(value.clone()),
                            AcpConfigValue::Boolean(_) => None,
                        };
                    }
                    guard.replace_config_options(response.config_options);
                    Ok(guard.snapshot.clone())
                }
            };
            drop(guard);
            let _ = reply_tx.send(result);
        }
        AcpControlMsg::SetMode {
            generation: requested_generation,
            mode_id,
            reply_tx,
        } => {
            if requested_generation != generation {
                let _ = reply_tx.send(Err(
                    "ACP session generation changed before its mode was applied".to_string(),
                ));
                return;
            }
            let advertised = shared
                .lock()
                .unwrap()
                .snapshot
                .modes
                .iter()
                .any(|mode| mode.id == mode_id);
            if !advertised {
                let _ = reply_tx.send(Err(format!(
                    "ACP session does not advertise mode '{mode_id}'"
                )));
                return;
            }
            let response = acp_timeout(
                "session/set_mode",
                ACP_HANDSHAKE_TIMEOUT,
                connection
                    .send_request(acp::SetSessionModeRequest::new(
                        session_id.clone(),
                        mode_id.clone(),
                    ))
                    .block_task(),
            )
            .await;
            let result = response.map(|_| {
                let mut guard = shared.lock().unwrap();
                guard.snapshot.current_mode = Some(mode_id);
                guard.snapshot.clone()
            });
            let _ = reply_tx.send(result);
        }
        AcpControlMsg::Steer {
            generation: requested_generation,
            text,
            attachments,
            reply_tx,
        } => {
            if requested_generation != generation {
                let _ = reply_tx.send(Err(
                    "ACP session generation changed before steering was applied".to_string(),
                ));
                return;
            }
            if !shared.lock().unwrap().snapshot.steering {
                let _ = reply_tx.send(Err(
                    "this ACP runtime did not advertise mid-turn steering".to_string()
                ));
                return;
            }
            // The same blocks a prompt would carry: the adapter runs a steer
            // through the very conversion `session/prompt` uses.
            let (capabilities, room, inline) = {
                let shared = shared.lock().unwrap();
                (shared.snapshot.prompt.clone(), shared.attachments_dir.clone(), shared.inline_budget)
            };
            let mut prompt = Vec::new();
            if !text.trim().is_empty() {
                prompt.push(acp::ContentBlock::Text(acp::TextContent::new(text)));
            }
            prompt.extend(native_attachment_blocks(attachments, &capabilities, room.as_deref(), inline));
            let params = serde_json::json!({
                "sessionId": session_id,
                "prompt": prompt,
                "_meta": {"steering": {"idleBehavior": "promptRequired"}},
            });
            // An extension method carries its leading underscore on the wire.
            let request = match UntypedMessage::new("_session/steering", params) {
                Ok(request) => request,
                Err(error) => {
                    let _ = reply_tx.send(Err(error.to_string()));
                    return;
                }
            };
            let response = acp_timeout(
                "_session/steering",
                ACP_HANDSHAKE_TIMEOUT,
                connection.send_request(request).block_task(),
            )
            .await;
            let result = response.and_then(|value| {
                match value.get("outcome").and_then(serde_json::Value::as_str) {
                    Some("injected") => Ok(AcpSteerOutcome::Injected),
                    Some("promptRequired") => Ok(AcpSteerOutcome::PromptRequired {
                        reason: value
                            .get("reason")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                    }),
                    other => Err(format!(
                        "ACP _session/steering returned unknown outcome {}",
                        other.unwrap_or("<missing>")
                    )),
                }
            });
            let _ = reply_tx.send(result);
        }
    }
}

/// Image types the Claude adapter (and the API behind it) takes as images.
/// Anything else that calls itself an image (HEIC, TIFF, SVG) is not sent as
/// one; the composer converts pasted images to PNG or JPEG first.
const ACP_IMAGE_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Every attachment reaches the runtime: each file is written where its room
/// reads it (`room`) and named there by its path; an image (or audio) the
/// session advertises also goes as itself, and text inline. Nothing is
/// refused, and nothing is left to an adapter to drop: the Claude adapter,
/// for one, silently ignores an embedded binary resource, so a PDF or a zip
/// sent that way simply vanished.
fn native_attachment_blocks(
    attachments: Vec<bakery_agent::NativeAttachment>,
    capabilities: &AcpPromptCapabilities,
    room: Option<&Path>,
    inline: usize,
) -> Vec<acp::ContentBlock> {
    let reads_natively = |media: &str| {
        (capabilities.image && ACP_IMAGE_TYPES.contains(&media))
            || (capabilities.audio && media.starts_with("audio/"))
    };
    let at = room.map(|room| crate::attachments::into_room(room, &attachments)).unwrap_or_default();
    let resolved = crate::attachments::resolve_files_within(attachments, &at, &reads_natively, inline);
    let mut blocks = Vec::with_capacity(resolved.native.len() + 1);
    if !resolved.text.is_empty() {
        blocks.push(acp::ContentBlock::Text(acp::TextContent::new(format!(
            "{}{}",
            crate::attachments::ATTACHED_PREAMBLE,
            resolved.text
        ))));
    }
    for file in resolved.native {
        let data = bakery_agent::tool_wire::encode_base64(&file.data);
        blocks.push(if file.mime_type.starts_with("audio/") {
            acp::ContentBlock::Audio(acp::AudioContent::new(data, file.mime_type))
        } else {
            acp::ContentBlock::Image(
                acp::ImageContent::new(data, file.mime_type)
                    .uri(format!("bakery-attachment:///{}", file.filename)),
            )
        });
    }
    blocks
}

// ─── Persistent session thread ───────────────────────────────────────────────

fn session_main(
    state: AppState,
    stream_id: String,
    seat: Seat,
    model_id: String,
    generation: u64,
    closed: Arc<AtomicBool>,
    workspace: PathBuf,
    config: AcpSpawnConfig,
    mut prompt_rx: tokio::sync::mpsc::Receiver<AcpPromptMsg>,
    mut control_rx: tokio::sync::mpsc::Receiver<AcpControlMsg>,
    shared: Arc<Mutex<AcpSessionShared>>,
    ready_tx: tokio::sync::oneshot::Sender<Result<u32, String>>,
) {
    let key = (stream_id.clone(), seat);

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(e.to_string()));
            remove_acp_slot_generation(&state, &key, generation);
            return;
        }
    };
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let cache_root = state.base.join("cache");

        // We split setup and loop so the compiler can reason about moves
        // cleanly (the kill closure is !Send, that's fine on LocalSet).
        let text_flushed = Arc::new(AtomicUsize::new(0));
        let subagent_calls: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
        let subagent_text: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
        let tool_titles: Arc<Mutex<BTreeMap<String, String>>> = Arc::default();
        let commands: Arc<Mutex<BTreeMap<String, NativeCommand>>> = Arc::default();
        let thinking: Arc<Mutex<Thinking>> = Arc::default();
        let setup_result: Result<_, String> = (|| async {
            if closed.load(Ordering::Acquire) {
                return Err("ACP session was closed before spawn".to_string());
            }
            // Thin room, always: ACP admission never depends on mise.toml
            // (07b) — pinned project toolchains are a separate, optional
            // capability (`bakery_exec::provision_project_tools`), not a
            // gate on whether the brain can boot at all.
            let env = bakery_exec::ensure_thin_room(&workspace, &cache_root, &stream_id)
                .map_err(|e| format!("ACP room: {e}"))?;
            let staged = bakery_exec::stage_acp_binary(&config.binary, &cache_root)?;
            // 07d-B Phase 2 Stage C item 3: the MCP shim command is a
            // SEPARATE binary from the ACP CLI (this same node's own
            // `bakery-node`, invoked `mcp-shim`) — the ACP child's Seatbelt
            // process-exec allowlist only ever covers `staged`'s own
            // directory (`acp_seatbelt_profile`'s doc comment), so the shim
            // must be staged (content-addressed, read-only) the same way,
            // and its directory added to the child's allowed-exec set below.
            // Otherwise a real ACP CLI trying to spawn the shim would hit
            // exactly the sandbox refusal this staging exists to avoid.
            let staged_mcp: Option<std::path::PathBuf> = config
                .mcp
                .as_ref()
                .map(|mcp| bakery_exec::stage_acp_binary(&mcp.command, &cache_root))
                .transpose()?;
            let mut extra_process_exec_dirs: Vec<std::path::PathBuf> = staged_mcp
                .as_deref()
                .and_then(Path::parent)
                .into_iter()
                .map(Path::to_path_buf)
                .collect();
            extra_process_exec_dirs.extend(config.extra_process_exec_dirs.iter().cloned());
            let seat_home = bakery_exec::seat_acp_home(&cache_root, &stream_id, seat.as_str());

            // Create seat HOME early so an explicitly named CLI config can be
            // copied to the exact path the CLI expects inside that HOME.
            std::fs::create_dir_all(&seat_home).map_err(|e| format!("ACP seat HOME: {e}"))?;
            if let Some(ref copy) = config.config_copy {
                let host_home = state.base.parent().ok_or_else(|| {
                    "Setup needed: Bakery base has no host HOME parent".to_string()
                })?;
                copy_config_into_seat_home(copy, &seat_home, host_home, &state.base)?;
            }

            let trusted = config.trusted;

            let pending_guard = crate::state::admit_acp_child(&state, &stream_id, trusted)
                .map_err(|e| format!("ACP admit: {e}"))?;
            // The room writes the workspace only where commands may: never in
            // place on the Bakery's own repo (rule 1), whatever the trust.
            let write_workspace = trusted && crate::tools::commands_write_workspace(&state, &stream_id);

            // Operator-granted machine access (Baker conversations only),
            // fixed into this runtime's sandbox for its whole life.
            let grants = crate::machine_access::for_stream(&state, &stream_id);
            let reach = crate::machine_access::reach_for(&state, &stream_id);
            if closed.load(Ordering::Acquire) {
                return Err("ACP session was closed before spawn".to_string());
            }
            // Where the stream's attachments are written for its room to read
            // (a container mounts it at the same path).
            let attachments_dir = crate::attachments::room_dir(&state, &stream_id);
            std::fs::create_dir_all(&attachments_dir).map_err(|e| format!("{}: {e}", attachments_dir.display()))?;
            // A Container or Locked reach runs the seat in a Linux container,
            // which gets the node's MCP over HTTP at its gateway.
            let mut container_mcp: Option<String> = None;
            // Where a runtime on this Mac keeps its sessions, for the
            // Bakery's own copy (`resume::keep_session`).
            let mut session_roots: Vec<(String, std::path::PathBuf)> = Vec::new();
            let handle = if matches!(
                reach,
                crate::machine_access::Reach::Container | crate::machine_access::Reach::Locked
            ) {
                let started = crate::seat_container::spawn(
                    &state, &stream_id, seat, &model_id, &workspace, write_workspace, &grants, reach,
                )?;
                container_mcp = Some(started.mcp_url);
                started.handle
            } else {
                extra_process_exec_dirs.extend(grants.exec);
                let mut rw_dirs = config.credential_dirs.clone();
                rw_dirs.extend(grants.write);
                if grants.whole_machine.is_some() {
                    // Host full: git works in a linked worktree whose repository
                    // sits inside Bakery's own (otherwise denied) folder.
                    if let Some((shared, own)) = crate::git::linked_worktree_git_dirs(&workspace) {
                        rw_dirs.extend([shared, own]);
                    }
                }
                let mut extra_env = config.extra_env.clone();
                extra_env.extend(grants.env);
                // A seat with a per-thread minted sign-in (the subscription,
                // or Claude native's own console/OpenRouter key) keeps the
                // stand-in its spawn config minted. A thread-wide API key for
                // the same vendor would switch it off, and a real token must
                // not replace the stand-in.
                let provider = crate::claude_providers::for_seat(&state, &stream_id, seat);
                if let Some(sub) = crate::swap::subscription_for_seat(&state, &model_id, provider.id()) {
                    extra_env.retain(|(key, value)| sub.keep_env(key, value));
                    let pinned: Vec<_> = config
                        .extra_env
                        .iter()
                        .filter(|(key, _)| sub.pins_env(key))
                        .cloned()
                        .collect();
                    extra_env.retain(|(key, _)| !sub.pins_env(key));
                    extra_env.extend(pinned);
                    if sub.name() == "grok-cli" {
                        let door = crate::doors::door(&state, &stream_id)?;
                        let minted = crate::swap::subscription_env(
                            &state,
                            &stream_id,
                            "acp:grok",
                            "subscription",
                            door,
                        );
                        extra_env.retain(|(key, _)| !sub.pins_env(key) && key != "GROK_HOME" && key != "GROK_AUTH_PATH" && key != "GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL");
                        extra_env.extend(minted);
                        let home = seat_home.join("seat-config");
                        let stand_in = extra_env
                            .iter()
                            .find(|(key, _)| key == "GROK_CLI_STAND_IN")
                            .map(|(_, value)| value.clone())
                            .unwrap_or_default();
                        crate::grok_cli::write_stand_in(&home, &stand_in)?;
                        let base = extra_env
                            .iter()
                            .find(|(key, _)| key == "GROK_CLI_CHAT_PROXY_BASE_URL")
                            .map(|(_, value)| value.clone())
                            .unwrap_or_default();
                        extra_env.push(("GROK_HOME".into(), home.display().to_string()));
                        extra_env.push(("GROK_AUTH_PATH".into(), home.join("auth.json").display().to_string()));
                        extra_env.push(("GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL".into(), base));
                    }
                }
                session_roots = crate::resume::session_roots(&seat_home, &extra_env);
                // The conversation's memory, in this runtime's own form.
                crate::routes::memory::render_native(&state, &stream_id, seat, &workspace, &seat_home, &extra_env);
                let host_home = state.base.parent().unwrap_or(state.base.as_ref());
                let hook_ro = crate::toolkit::inject_acp_hooks(&state, &stream_id, &model_id, &seat_home, host_home, &mut extra_env)
                    .map_err(|e| format!("ACP hook injection: {e}"))?;
                let mut read_only = config.read_only.clone();
                read_only.extend(hook_ro);
                // The stream's attachments: read-only to its room.
                let mut read = grants.read.clone();
                read.push(attachments_dir.clone());
                bakery_exec::spawn_boxed_acp(
                    &env,
                    &cache_root,
                    &staged,
                    &seat_home,
                    &workspace,
                    &grants.deny,
                    &extra_env,
                    &config.argv_suffix,
                    write_workspace,
                    &extra_process_exec_dirs,
                    &rw_dirs,
                    &read,
                    grants.whole_machine.as_ref(),
                    &read_only,
                )?
            };
            // The seat's own temp folder: a container's /tmp, or the room's
            // (its TMPDIR and CLAUDE_CODE_TMPDIR) on this Mac.
            let seat_tmp = if matches!(reach, crate::machine_access::Reach::Container | crate::machine_access::Reach::Locked) {
                vec![PathBuf::from("/tmp")]
            } else {
                let tmp = seat_home.join("tmp");
                let canonical = tmp.canonicalize().ok().filter(|canonical| *canonical != tmp);
                std::iter::once(tmp).chain(canonical).collect()
            };
            {
                let mut guard = shared.lock().unwrap();
                guard.attachments_dir = Some(attachments_dir);
                guard.inline_budget = crate::attachments::inline_budget(&model_id);
                guard.seat_tmp = seat_tmp;
            }
            let pid = handle.pid;
            let child_guard = pending_guard.promote(pid);

            let (_, stdin, stdout, stderr, kill_fn) = handle.into_io_and_killer();
            // Arm kill-on-drop immediately after taking the kill closure, so
            // any ? between here and disarm() kills the child rather than
            // leaking it (the child's Drop only waits without SIGKILL).
            let kill_guard = KillGuard::arm(kill_fn);

            if closed.load(Ordering::Acquire) {
                return Err("ACP session was closed during spawn".to_string());
            }

            // tokio::fs::File wraps blocking I/O via spawn_blocking. On a
            // single-threaded LocalSet, spawn_blocking tasks run on tokio's
            // separate blocking thread pool — they do NOT block the async
            // executor. This is correct for pipe fds: each read/write runs on
            // a blocking thread while the LocalSet continues scheduling.
            let stdin_async = {
                use std::os::unix::io::IntoRawFd;
                let std_file = unsafe { std::fs::File::from_raw_fd(stdin.into_raw_fd()) };
                tokio::fs::File::from_std(std_file).compat_write()
            };
            // ACP stdio is newline-delimited JSON. Bound each frame, not the
            // lifetime or whole turn, so a persistent tool-heavy session can
            // carry many legitimate bounded messages without exhausting a
            // cumulative byte allowance.
            let stdout_async = {
                use std::os::unix::io::IntoRawFd;
                let std_file = unsafe { std::fs::File::from_raw_fd(stdout.into_raw_fd()) };
                FrameLimitedAsyncRead {
                    inner: tokio::fs::File::from_std(std_file).compat(),
                    remaining: MAX_ACP_FRAME,
                    cap: MAX_ACP_FRAME,
                }
            };

            // Drain stderr, however much the runtime writes: nothing it says
            // ends it.
            let stderr_task = {
                let stderr_file = {
                    use std::os::unix::io::IntoRawFd;
                    let std_file = unsafe { std::fs::File::from_raw_fd(stderr.into_raw_fd()) };
                    tokio::fs::File::from_std(std_file)
                };
                tokio::task::spawn_local(async move {
                    use tokio::io::AsyncReadExt;
                    let mut stderr = stderr_file;
                    let mut buf = vec![0u8; 8192];
                    loop {
                        match stderr.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                })
            };

            let text = Arc::new(Mutex::new(String::new()));
            let current_turn_id = Arc::new(Mutex::new(String::new()));
            let current_delta_tx = Arc::new(Mutex::new(None));
            let client = Arc::new(GateClient {
                state: state.clone(),
                stream_id: stream_id.clone(),
                seat,
                generation,
                model_id: model_id.clone(),
                text: text.clone(),
                text_flushed: text_flushed.clone(),
                current_turn_id: current_turn_id.clone(),
                current_delta_tx: current_delta_tx.clone(),
                shared: shared.clone(),
                trusted,
                commands: commands.clone(),
                subagent_calls: subagent_calls.clone(),
                subagent_text: subagent_text.clone(),
                tool_titles: tool_titles.clone(),
                thinking: thinking.clone(),
                compactions: Mutex::new(BTreeMap::new()),
            });

            let (connection_tx, connection_rx) = tokio::sync::oneshot::channel();
            let io_task = tokio::task::spawn_local(serve_gate_client(
                client,
                agent_client_protocol::ByteStreams::new(stdin_async, stdout_async),
                connection_tx,
            ));
            let connection = connection_rx
                .await
                .map_err(|_| "ACP connection closed before it started".to_string())?;

            let client_capabilities = acp::ClientCapabilities::new()
                // Bakery does not implement the ACP terminal tool family;
                // auth terminal support is a distinct negotiated bit.
                .terminal(false)
                .auth(acp::AuthCapabilities::new().terminal(true))
                // Stable ACP only lets an adapter offer a boolean option (a
                // fast-mode toggle, say) to a client that says it handles them.
                .session(
                    acp::ClientSessionCapabilities::new()
                        .config_options(
                            acp::SessionConfigOptionsCapabilities::new()
                                .boolean(acp::BooleanConfigOptionCapabilities::new()),
                        )
                        // The runtime's own compactions, as compaction
                        // updates with their summary (`record_compaction`).
                        .compaction(acp::CompactionCapabilities::new()),
                )
                // Form elicitation is how a native runtime asks the human
                // questions (Claude's AskUserQuestion is off without it).
                .elicitation(
                    acp::ElicitationCapabilities::new().form(acp::ElicitationFormCapabilities::new()),
                )
                // Claude's adapter reports a command's real output and exit
                // code only to a client that says it shows them (as
                // `_meta.terminal_*` on the tool call); without this the
                // operator saw "running Terminal" and nothing of the command.
                // Running it stays the runtime's own sandboxed shell.
                // `subagent-transcript`: a subagent's own words arrive tagged
                // with the call that launched it, so Bakery shows them under
                // that call instead of losing them or mixing them into the
                // reply.
                .meta(serde_json::Map::from_iter(
                    ["terminal_output", "terminal_output_delta", "subagent-transcript"]
                        .map(|key| (key.to_string(), serde_json::Value::Bool(true))),
                ));
            // Sent untyped so the raw response survives next to the typed
            // one: only it still says which auth methods are `env_var`.
            let initialize = UntypedMessage::new(
                "initialize",
                acp::InitializeRequest::new(agent_client_protocol::schema::ProtocolVersion::V1)
                    .client_capabilities(client_capabilities)
                    .client_info(acp::Implementation::new(
                        "bakery-node",
                        env!("CARGO_PKG_VERSION"),
                    )),
            )
            .map_err(|e| format!("ACP initialize failed: {e}"))?;
            let raw_init = acp_timeout(
                "initialize",
                ACP_HANDSHAKE_TIMEOUT,
                connection.send_request(initialize).block_task(),
            )
            .await?;
            let init: acp::InitializeResponse = serde_json::from_value(raw_init.clone())
                .map_err(|e| format!("ACP initialize returned an invalid response: {e}"))?;
            let env_var_methods = env_var_auth_methods(&raw_init);
            {
                let mut guard = shared.lock().unwrap();
                guard.snapshot.runtime = init.agent_info.as_ref().map(|info| AcpRuntimeIdentity {
                    name: info.name.clone(),
                    title: info.title.clone(),
                    version: info.version.clone(),
                });
                guard.snapshot.auth_methods = init
                    .auth_methods
                    .iter()
                    .map(|method| project_auth_method(method, &env_var_methods))
                    .collect();
                guard.snapshot.prompt = AcpPromptCapabilities {
                    image: init.agent_capabilities.prompt_capabilities.image,
                    audio: init.agent_capabilities.prompt_capabilities.audio,
                    embedded_context: init.agent_capabilities.prompt_capabilities.embedded_context,
                };
                guard.snapshot.steering = steering_supported(&init);
                guard.snapshot.native_review = native_review_supported(&init);
                guard.review_reasons = init
                    .meta
                    .as_ref()
                    .and_then(|meta| meta.get(NATIVE_REVIEW_META_KEY))
                    .and_then(|review| review.get("reasons"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true);
            }
            if let Some(auth_method) = select_agent_auth_method(&init, &env_var_methods)? {
                acp_timeout(
                    "authenticate",
                    ACP_HANDSHAKE_TIMEOUT,
                    connection
                        .send_request(acp::AuthenticateRequest::new(auth_method))
                        .block_task(),
                )
                .await?;
            }
            // 07d-B Phase 2 Stage C item 3: MCP delivery for the permanent
            // Baker / a task worker's session — `Some` only when
            // `config.mcp` was actually built for this stream (never a
            // caller-supplied override; see `state::resolve_model_id_for_seat`).
            // The command handed to the ACP child is the STAGED copy
            // (`staged_mcp`, computed above) — the one path the child's own
            // Seatbelt profile actually allows it to exec — never
            // `mcp.command` (this process's live, unstaged binary path).
            let mcp_token = config.mcp.as_ref().and_then(|mcp| {
                mcp.env
                    .iter()
                    .find(|(k, _)| k == "BAKERY_MCP_TOKEN")
                    .map(|(_, v)| v.clone())
            });
            let mut mcp_servers: Vec<acp::McpServer> = match (&config.mcp, &staged_mcp) {
                // A container seat reaches the node's MCP over HTTP at its
                // gateway, with this session's own token.
                (Some(_), _) if container_mcp.is_some() => vec![acp::McpServer::Http(
                    acp::McpServerHttp::new("bakery", container_mcp.clone().unwrap_or_default())
                        .headers(vec![acp::HttpHeader::new(
                            "Authorization",
                            format!("Bearer {}", mcp_token.clone().unwrap_or_default()),
                        )]),
                )],
                (Some(mcp), Some(staged_mcp)) => vec![acp::McpServer::Stdio(
                    acp::McpServerStdio::new("bakery", staged_mcp.clone())
                        .args(mcp.args.clone())
                        .env(
                            mcp.env
                                .iter()
                                .map(|(k, v)| acp::EnvVariable::new(k.clone(), v.clone()))
                                .collect(),
                        ),
                )],
                _ => vec![],
            };
            // The conversation's own MCP servers, from the Bakery's toolkit.
            for server in crate::toolkit::mcp_servers(&state, &stream_id) {
                mcp_servers.push(match (&server.command, &server.url) {
                    (Some(command), _) => acp::McpServer::Stdio(
                        acp::McpServerStdio::new(server.name.clone(), std::path::PathBuf::from(command)).args(server.args.clone()),
                    ),
                    (None, Some(url)) => acp::McpServer::Http(acp::McpServerHttp::new(server.name.clone(), url.clone())),
                    (None, None) => continue,
                });
            }
            // The Bakery holds each native session (its id, and a copy of
            // what the runtime keeps of it): one the runtime can resume picks
            // its conversation back up; only a new one needs the catch-up.
            let agent_id = crate::node::agent_id(&stream_id, seat);
            let earlier = init
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some()
                .then(|| crate::resume::last_session(&state, &stream_id, &agent_id, &model_id))
                .flatten()
                // One its runtime never kept can't resume: a fresh one, unasked.
                .filter(|id| crate::resume::kept(&state, &stream_id, &agent_id, id));
            let mut resumed = None;
            if let Some(id) = earlier {
                crate::resume::restore_session(&state, &agent_id, &id, &session_roots);
                let ask = acp::ResumeSessionRequest::new(id.clone(), workspace.clone())
                    .mcp_servers(mcp_servers.clone());
                // A long session takes its time to load: waited for as long
                // as it takes within `ACP_RESUME_TIMEOUT`, unless the session
                // closes meanwhile or the operator stops the turn waiting on it.
                let resume = acp_timeout("session/resume", ACP_RESUME_TIMEOUT, connection.send_request(ask).block_task());
                let stopped = || {
                    crate::state::agent_activity_snapshot(&state, &agent_id).is_some_and(|activity| activity.cancel_requested)
                };
                let closing = async {
                    while !closed.load(Ordering::Acquire) && !stopped() {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                };
                let outcome = tokio::select! {
                    outcome = resume => outcome,
                    () = closing => return Err("ACP session was closed or stopped while its last session resumed".to_string()),
                };
                match outcome {
                    // The operator chose a fresh session while this one
                    // resumed: their choice wins over this attempt, whichever
                    // way it went. It opens the fresh one itself, and asks
                    // nothing more.
                    _ if crate::resume::last_session(&state, &stream_id, &agent_id, &model_id).as_deref() != Some(id.as_str()) => {}
                    Ok(back) => resumed = Some((acp::SessionId::new(id), back.modes, back.config_options)),
                    // Said on the thread, and the turn stops there: the
                    // operator chooses to try it again or start fresh
                    // (`agent.session.chosen`), never the node on its own.
                    Err(e) => {
                        eprintln!("bakery-node: {stream_id}: its last native session did not resume: {e}");
                        crate::resume::note_not_resumed(&state, &stream_id, &agent_id, &model_id, &id, &e);
                        return Err(format!(
                            "{} ({e}). Try it again, or start fresh: a new session, briefed from this thread.",
                            crate::resume::NOT_RESUMED_ERROR
                        ));
                    }
                }
            }
            let resumed_session = resumed.is_some();
            let (session_id, session_modes, session_options) = match resumed {
                Some(back) => back,
                None => {
                    let new_session = acp_timeout(
                        "session/new",
                        ACP_HANDSHAKE_TIMEOUT,
                        connection
                            .send_request(
                                acp::NewSessionRequest::new(workspace.clone()).mcp_servers(mcp_servers),
                            )
                            .block_task(),
                    )
                    .await?;
                    (new_session.session_id, new_session.modes, new_session.config_options)
                }
            };
            crate::resume::note_session(&state, &stream_id, &agent_id, &model_id, &session_id.0, resumed_session);

            {
                let mut guard = shared.lock().unwrap();
                if let Some(modes) = &session_modes {
                    guard.snapshot.current_mode = Some(modes.current_mode_id.0.to_string());
                    guard.snapshot.modes = modes
                        .available_modes
                        .iter()
                        .map(|mode| AcpMode {
                            id: mode.id.0.to_string(),
                            name: mode.name.clone(),
                            description: mode.description.clone(),
                        })
                        .collect();
                }
                guard.replace_config_options(session_options.clone().unwrap_or_default());
            }
            // Every real session teaches the node what this account offers,
            // so the next model picker has a list without waiting for one.
            let learned = shared.lock().unwrap().snapshot.clone();
            crate::account_adapter::remember_native_catalog(&state, &learned);

            // A restarted native child receives a new ACP generation. Replay
            // only this agent's prior acknowledged selections, and only after
            // its fresh `session/new` response has made the current option
            // vocabulary available for validation.
            restore_durable_native_config(
                &connection,
                &session_id,
                &shared,
                &state,
                &stream_id,
                seat,
                &model_id,
            )
            .await;

            if closed.load(Ordering::Acquire) {
                return Err("ACP session was closed during handshake".to_string());
            }

            let kill_fn = kill_guard.disarm();
            Ok((
                session_id,
                (session_roots, resumed_session),
                text,
                connection,
                io_task,
                pid,
                child_guard,
                kill_fn,
                stderr_task,
                current_turn_id,
                current_delta_tx,
            ))
        })()
        .await;

        let (
            session_id,
            (session_roots, resumed_session),
            text,
            connection,
            mut io_task,
            pid,
            child_guard,
            kill_fn,
            stderr_task,
            current_turn_id,
            current_delta_tx,
        ) = match setup_result {
            Ok(v) => {
                let _ = ready_tx.send(Ok(v.5));
                v
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                remove_acp_slot_generation(&state, &key, generation);
                return;
            }
        };

        // A brand-new (or freshly respawned) session has no memory of any
        // prior turn on this agent_id — the CLI's own conversational state
        // died with whatever process last held it. Prepended to the FIRST
        // prompt only; a session that stays live across many turns must not
        // re-inject the same history on every single one.
        let mut first_prompt = !resumed_session;
        let agent_id = crate::node::agent_id(&stream_id, seat);
        // The Bakery's copy of the session catches up with what the runtime
        // wrote after the last turn ended (it writes its transcript then).
        let keep = || crate::resume::keep_session(&state, &agent_id, &session_id.0, &session_roots);
        // Then the compactions it recorded give theirs their size; as the
        // next prompt starts, any still without one has none.
        let measure = |settle: bool| fill_compaction_sizes(&state, &stream_id, seat, &session_id.0, settle);

        loop {
            if closed.load(Ordering::Acquire) {
                break;
            }
            let msg = loop {
                tokio::select! {
                    msg = prompt_rx.recv() => break msg,
                    control = control_rx.recv() => match control {
                        Some(control) => {
                            handle_acp_control(
                                &connection,
                                &session_id,
                                generation,
                                &shared,
                                control,
                            ).await;
                        }
                        None => break None,
                    },
                    _ = &mut io_task => break None,
                }
            };
            let Some(msg) = msg else { break };
            keep();
            measure(true);
            text.lock().unwrap().clear();
            text_flushed.store(0, Ordering::Relaxed);
            subagent_calls.lock().unwrap().clear();
            subagent_text.lock().unwrap().clear();
            tool_titles.lock().unwrap().clear();
            thinking.lock().unwrap().begin();
            *current_turn_id.lock().unwrap() = msg.turn_id.clone();
            *current_delta_tx.lock().unwrap() = Some(msg.delta_tx.clone());
            // A command reaches the runtime exactly as typed; the Bakery's own
            // context waits for the next message.
            let prompt_text = if first_prompt && !is_command(&msg.text) {
                first_prompt = false;
                let persona = acp_persona_preamble(&state, &stream_id, seat);
                // The memory's index, once per session: the same for all of
                // it, as the runtime's prompt cache wants.
                let memory = crate::memory_index::turn_index(&state, &stream_id, crate::memory_index::INDEX_TOKENS).unwrap_or_default();
                let catchup = acp_catchup_context(&state, &stream_id, seat, &msg.turn_id);
                if persona.is_empty() && memory.is_empty() && catchup.is_empty() {
                    msg.text
                } else {
                    format!("{persona}{memory}{catchup}{}", msg.text)
                }
            } else {
                msg.text
            };
            let (prompt_capabilities, cost_before, room, inline) = {
                let shared = shared.lock().unwrap();
                (shared.snapshot.prompt.clone(), shared.cost.clone(), shared.attachments_dir.clone(), shared.inline_budget)
            };
            let mut prompt_blocks =
                vec![acp::ContentBlock::Text(acp::TextContent::new(prompt_text))];
            prompt_blocks.extend(native_attachment_blocks(msg.attachments, &prompt_capabilities, room.as_deref(), inline));
            // Polled in place below, never dropped while the prompt is live:
            // dropping it would send `$/cancel_request` and cancel the turn.
            // Every path that does drop it early also kills the child.
            // No time limit: a turn runs as long as its work takes, as in
            // Claude Code itself; the operator stops it.
            let turn_future = async {
                connection
                    .send_request(acp::PromptRequest::new(session_id.clone(), prompt_blocks))
                    .block_task()
                    .await
                    .map_err(|e| format!("ACP session/prompt failed: {e}"))
            };
            tokio::pin!(turn_future);
            let turn_result = loop {
                tokio::select! {
                    result = &mut turn_future => break result,
                    control = control_rx.recv() => match control {
                        Some(control) => {
                            handle_acp_control(
                                &connection,
                                &session_id,
                                generation,
                                &shared,
                                control,
                            ).await;
                        }
                        None => break Err("ACP control channel closed during prompt".to_string()),
                    },
                    _ = &mut io_task => break Err("ACP transport closed during prompt".to_string()),
                }
            };
            // A failed `session/prompt` kills the child and fails the turn
            // (07d-A5 fix-verification review): left alive and reused for a
            // later turn, the child's leftover work could still emit a
            // `request_permission`/`ToolCall` notification tagged (by
            // `GateClient`'s `current_turn_id`, read at callback-fire time)
            // with the NEWER turn's id — re-opening the activity-phase
            // clobbering `admit_child_tool_call`'s turn_id staleness guard
            // exists to close. A later turn always gets a fresh session.
            let reply = if let Err(e) = turn_result {
                bakery_exec::kill_process_group(pid);
                Err(e)
            } else {
                turn_result.map(|response| {
                    let shared = shared.lock().unwrap();
                    AcpPromptReply {
                        text: text.lock().unwrap().clone(),
                        cancelled: response.stop_reason == acp::StopReason::Cancelled,
                        usage: native_turn_usage(
                            response.usage.as_ref(),
                            cost_before.as_ref(),
                            shared.cost.as_ref(),
                            shared.snapshot.usage.as_ref(),
                        ),
                    }
                })
            };
            let fatal = reply.is_err();
            // The rest of the text lands before the route records the turn's
            // end. A turn already split around tool calls records it as the
            // last part; a stopped or failed one keeps its partial words.
            // A plain completed reply is left whole to `agent.turn.done`.
            let completed = matches!(&reply, Ok(r) if !r.cancelled);
            // Output its command calls still held: recorded, however it ended.
            record_held_output(&state, &stream_id, seat, &model_id, &commands, None);
            // Stop cut short the calls still open: they had started (one held
            // at a card or a review is settled as it was stopped).
            if stop_asked(&state, &stream_id, seat, &msg.turn_id) {
                record_stopped_calls(&state, &stream_id, seat, &model_id, &msg.turn_id, &tool_titles);
            }
            // Words of a subagent still running when the turn ended.
            let unfinished: Vec<String> = subagent_text.lock().unwrap().keys().cloned().collect();
            for parent in unfinished {
                flush_subagent_text(
                    &state,
                    &stream_id,
                    seat,
                    &model_id,
                    &msg.turn_id,
                    &parent,
                    &subagent_text,
                );
            }
            if !completed || text_flushed.load(Ordering::Relaxed) > 0 {
                flush_acp_text(
                    &state,
                    &stream_id,
                    seat,
                    &model_id,
                    &msg.turn_id,
                    &text,
                    &text_flushed,
                    true,
                );
            }
            // Thinking the turn ended on lands too (W1-T).
            let last = thinking.lock().unwrap().settle();
            if let Some(stretch) = last {
                record_thought(&state, &stream_id, seat, &model_id, &msg.turn_id, stretch.fact());
            }
            *current_delta_tx.lock().unwrap() = None;
            *current_turn_id.lock().unwrap() = String::new();
            // The turn ends first; the copy and the compaction sizes follow,
            // before the next prompt is read.
            let _ = msg.reply_tx.send(reply);
            keep();
            measure(false);

            if fatal {
                // Mark this session dead immediately and atomically — a
                // second fix-verification round finding: merely killing the
                // OS process and waiting for `io_task` to eventually notice
                // (the check below) leaves a window where a concurrent or
                // immediately-following `chat()` call on this agent can
                // still acquire `prompt_lock`, find this slot registered as
                // `Live` (killing a process does not by itself flip
                // `closed` or evict the registry entry — both lag behind an
                // async SIGKILL and io_task's own EOF detection), and
                // enqueue another prompt onto a session whose child is
                // already dead or dying. Setting `closed` makes any
                // in-flight or racing `chat()`'s own `live.is_closed()`
                // check refuse immediately; evicting the registry entry
                // stops a fresh `ensure_boxed_session` call from finding
                // this slot at all — together, a retry arriving before the
                // killed pid has even been reaped still gets a genuinely
                // new session, never a race onto the dying one.
                closed.store(true, Ordering::Release);
                remove_acp_slot_generation(&state, &key, generation);
                break;
            }

            // If the transport is dead (frame overflow, the stderr
            // drain killed the child, or any other fatal I/O error not
            // otherwise caught above) stop looping — sending another
            // prompt would just time out.
            if io_task.is_finished() {
                break;
            }
        }

        io_task.abort();
        stderr_task.abort();
        (kill_fn)();
        drop(child_guard);
        keep();
        remove_acp_slot_generation(&state, &key, generation);
    });
}

// ─── Ensure a live session ───────────────────────────────────────────────────

fn ensure_boxed_session(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
    workspace: &PathBuf,
    config: &AcpSpawnConfig,
) -> Result<LiveAcpSession, String> {
    let key = (stream_id.to_string(), seat);

    // Someone else is starting this seat's session (or reaping the last one):
    // wait for that, then take the live session — or the empty slot, if their
    // start failed. Callers are blocking contexts (see `ready_rx` below).
    let deadline = std::time::Instant::now() + ACP_START_WAIT;
    let (generation, closed) = loop {
        {
            let mut sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&key) {
                Some(AcpSlot::Live(live)) => return Ok(live.clone()),
                Some(AcpSlot::Spawning { .. }) | Some(AcpSlot::Closing { .. }) => {}
                None => {
                    let generation = next_acp_generation();
                    let closed = Arc::new(AtomicBool::new(false));
                    sessions.insert(
                        key.clone(),
                        AcpSlot::Spawning {
                            generation,
                            closed: closed.clone(),
                        },
                    );
                    break (generation, closed);
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err("ACP session is still starting — retry shortly".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    };

    let (prompt_tx, prompt_rx) = tokio::sync::mpsc::channel::<AcpPromptMsg>(1);
    let (control_tx, control_rx) = tokio::sync::mpsc::channel::<AcpControlMsg>(16);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<u32, String>>();
    let shared = Arc::new(Mutex::new(AcpSessionShared::new(
        model_id.to_string(),
        generation,
    )));

    let state2 = state.clone();
    let stream_id2 = stream_id.to_string();
    let model_id2 = model_id.to_string();
    let workspace2 = workspace.clone();
    let config2 = config.clone();
    let closed2 = closed.clone();
    let shared2 = shared.clone();
    if let Err(e) = std::thread::Builder::new()
        .name("bakery-acp-session".to_string())
        .spawn(move || {
            session_main(
                state2, stream_id2, seat, model_id2, generation, closed2, workspace2, config2,
                prompt_rx, control_rx, shared2, ready_tx,
            )
        })
    {
        remove_acp_slot_generation(state, &key, generation);
        return Err(e.to_string());
    }

    let pid = match ready_rx.blocking_recv() {
        Ok(Ok(pid)) => pid,
        Ok(Err(e)) => {
            remove_acp_slot_generation(state, &key, generation);
            return Err(e);
        }
        Err(_) => {
            remove_acp_slot_generation(state, &key, generation);
            return Err("ACP session thread exited before ready signal".to_string());
        }
    };

    let live = LiveAcpSession {
        prompt_lock: Arc::new(Mutex::new(())),
        prompt_tx,
        control_tx,
        shared,
        closed: closed.clone(),
        generation,
        pid,
    };
    let published = {
        let mut sessions = state.acp_sessions.lock().unwrap();
        match sessions.get(&key) {
            Some(AcpSlot::Spawning {
                generation: current,
                ..
            }) if *current == generation && !closed.load(Ordering::Acquire) => {
                sessions.insert(key.clone(), AcpSlot::Live(live.clone()));
                true
            }
            _ => false,
        }
    };
    if published {
        Ok(live)
    } else {
        closed.store(true, Ordering::Release);
        bakery_exec::kill_process_group(pid);
        Err("ACP session was superseded while starting — retry shortly".to_string())
    }
}

// ─── Model seam ─────────────────────────────────────────────────────────────

/// Configuration for spawning a boxed ACP child. Carries the binary path,
/// any argv after the binary (e.g. `["agent", "stdio"]`), an optional
/// config directory to copy into seat HOME for auth, and per-spawn env vars.
#[derive(Clone)]
pub(crate) struct AcpSpawnConfig {
    pub(crate) binary: PathBuf,
    pub(crate) argv_suffix: Vec<String>,
    /// If set: copy this one named directory (once, idempotent) to the exact
    /// relative location the CLI expects inside its isolated seat HOME.
    pub(crate) config_copy: Option<AcpConfigCopyPlan>,
    pub(crate) extra_env: Vec<(String, String)>,
    /// True for trusted (non-Burn) streams: auto-approve all ACP tool calls
    /// and allow the seatbelt child to write the worktree. False for Burn
    /// streams (and for test models that want to exercise the gate).
    pub(crate) trusted: bool,
    /// The loopback MCP server to hand this session at `session/new`
    /// (07d-B Phase 2 Stage C item 3) — `Some` only for the permanent
    /// Baker's stream or a task socket (`crate::mcp::mcp_scope_for_stream`);
    /// every other stream keeps today's `mcp_servers: vec![]` behavior.
    pub(crate) mcp: Option<McpSpawnConfig>,
    /// Verified code-only directories the adapter may read and execute in
    /// addition to its staged entry binary (for example a pinned Node
    /// package tree). Never use this for HOME or credential storage.
    pub(crate) extra_process_exec_dirs: Vec<PathBuf>,
    /// Bakery-OWNED credential directories the child may read AND write, so a
    /// subscription adapter can refresh its own OAuth token in place. These
    /// must live under `~/.bakery` — never the operator's own `~/.claude` or
    /// `~/.codex`, which stay in `default_deny_paths` so a Bakery session can
    /// never rotate the refresh token out from under the host CLI.
    pub(crate) credential_dirs: Vec<PathBuf>,
    /// Inside those, what the runtime only reads and no command it runs may
    /// change: its settings (`bakery_exec::claude_settings`).
    pub(crate) read_only: Vec<PathBuf>,
}

/// One loopback MCP server to advertise at `session/new` — the downstream
/// ACP CLI spawns `command args...` itself with `env` set, exactly as
/// `agent_client_protocol::McpServerStdio` describes; this node never spawns
/// it directly.
#[derive(Clone)]
pub(crate) struct McpSpawnConfig {
    pub(crate) command: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub(crate) struct AcpConfigCopyPlan {
    pub(crate) source: PathBuf,
    pub(crate) home_relative_destination: PathBuf,
}

pub(crate) fn canonical_directory(path: &Path, label: &str) -> Result<PathBuf, String> {
    let canonical = path.canonicalize().map_err(|e| {
        format!(
            "Setup needed: {label} {} is unavailable: {e}",
            path.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(format!(
            "Setup needed: {label} {} is not a directory",
            canonical.display()
        ));
    }
    Ok(canonical)
}

pub(crate) fn normalized_home_destination(path: &Path) -> Result<PathBuf, String> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "Setup needed: ACP config destination {} must be relative to the seat HOME without traversal",
                    path.display()
                ));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err("Setup needed: ACP config destination must not be empty".to_string());
    }
    Ok(normalized)
}

pub(crate) fn validated_config_source(
    source: &Path,
    host_home: &Path,
    bakery_base: &Path,
) -> Result<PathBuf, String> {
    if source.as_os_str().is_empty() || !source.is_absolute() {
        return Err(
            "Setup needed: ACP config source must be a non-empty absolute directory path"
                .to_string(),
        );
    }
    let source = canonical_directory(source, "ACP config source")?;
    let host_home = canonical_directory(host_home, "host HOME")?;
    let bakery_base = canonical_directory(bakery_base, "Bakery base")?;

    if host_home.starts_with(&source) {
        return Err(
            "Setup needed: ACP config source must be one plug-specific directory, not host HOME or one of its ancestors"
                .to_string(),
        );
    }
    if source == bakery_base || source.starts_with(&bakery_base) {
        return Err(
            "Setup needed: ACP config source must never be ~/.bakery or anything beneath it"
                .to_string(),
        );
    }
    Ok(source)
}

fn checked_destination(seat_home: &Path, relative: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(seat_home).map_err(|e| {
        format!(
            "Setup needed: cannot create ACP seat HOME {}: {e}",
            seat_home.display()
        )
    })?;
    let home = canonical_directory(seat_home, "ACP seat HOME")?;
    let components: Vec<_> = relative.components().collect();
    let mut current = home.clone();

    for (index, component) in components.iter().enumerate() {
        let Component::Normal(part) = component else {
            return Err(
                "Setup needed: ACP config destination contains an invalid component".to_string(),
            );
        };
        let next = current.join(part);
        let last = index + 1 == components.len();
        match std::fs::symlink_metadata(&next) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(format!(
                        "Setup needed: ACP config destination component {} is not a real directory",
                        next.display()
                    ));
                }
                let canonical = canonical_directory(&next, "ACP config destination")?;
                if !canonical.starts_with(&home) {
                    return Err(
                        "Setup needed: ACP config destination escapes the seat HOME".to_string()
                    );
                }
                current = canonical;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && last => return Ok(next),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&next).map_err(|e| {
                    format!(
                        "Setup needed: cannot create ACP config destination {}: {e}",
                        next.display()
                    )
                })?;
                current = canonical_directory(&next, "ACP config destination")?;
                if !current.starts_with(&home) {
                    return Err(
                        "Setup needed: ACP config destination escapes the seat HOME".to_string()
                    );
                }
            }
            Err(e) => {
                return Err(format!(
                    "Setup needed: cannot inspect ACP config destination {}: {e}",
                    next.display()
                ));
            }
        }
    }
    Ok(current)
}

fn copy_config_into_seat_home(
    plan: &AcpConfigCopyPlan,
    seat_home: &Path,
    host_home: &Path,
    bakery_base: &Path,
) -> Result<(), String> {
    let source = validated_config_source(&plan.source, host_home, bakery_base)?;
    // A plug's login folder is not copied into the seat. The room holds a
    // stand-in; the node adds the real credential only toward that vendor's
    // hosts. Filenames decide this. The file contents are not read.
    if login_folder(&source) {
        return Ok(());
    }
    let relative = normalized_home_destination(&plan.home_relative_destination)?;
    let destination = checked_destination(seat_home, &relative)?;
    if destination.exists() {
        return Ok(());
    }
    copy_dir_all(&source, &destination).map_err(|e| {
        format!(
            "Setup needed: ACP config copy from {} failed: {e}",
            source.display()
        )
    })?;
    let canonical_home = canonical_directory(seat_home, "ACP seat HOME")?;
    let canonical_destination = canonical_directory(&destination, "ACP config destination")?;
    if !canonical_destination.starts_with(&canonical_home) {
        return Err("Setup needed: ACP config destination escapes the seat HOME".to_string());
    }
    Ok(())
}

/// A directory whose name is a vendor login home, or that holds a
/// credential-shaped file. Names only: nothing in it is opened.
fn login_folder(source: &std::path::Path) -> bool {
    let name = source.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if matches!(name, ".grok" | ".cursor" | ".codex" | ".claude" | "cursor") {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(source) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        bakery_exec::is_credential_filename(&path)
            || (entry.file_type().is_ok_and(|kind| kind.is_dir()) && login_folder(&path))
    })
}

fn copy_dir_all(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if bakery_exec::is_credential_filename(&entry.path()) {
            continue;
        }
        if ty.is_dir() {
            if login_folder(&entry.path()) {
                continue;
            }
            copy_dir_all(&entry.path(), &dst_path)?;
        } else if ty.is_file() {
            std::fs::copy(&entry.path(), &dst_path)?;
        }
        // Skip symlinks — avoid following paths out of the config dir.
    }
    Ok(())
}

#[cfg(test)]
mod login_copy {
    use super::login_folder;

    #[test]
    fn a_login_folder_is_not_copied_into_the_seat() {
        let dir = tempfile::tempdir().unwrap();
        let login = dir.path().join(".grok");
        std::fs::create_dir_all(&login).unwrap();
        std::fs::write(login.join("auth.json"), b"{\"key\":\"fixture\"}").unwrap();
        assert!(login_folder(&login));
        let cursor = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(cursor.join("cli-config.json"), b"{}").unwrap();
        assert!(login_folder(&cursor), "a .cursor directory is a login home even before its schema is known");
        let plain = dir.path().join("settings");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("settings.json"), b"{}").unwrap();
        assert!(!login_folder(&plain));
    }
}

pub(crate) struct AcpPipeModel {
    state: AppState,
    stream_id: String,
    seat: Seat,
    workspace: PathBuf,
    binary_override: Option<AcpSpawnConfig>,
    revoked: Arc<AtomicBool>,
    owned_generation: Arc<Mutex<Option<u64>>>,
    /// The exact model id this seat was assigned (`"acp"`, or a named plug's
    /// own id like `"acp:claude"`) — `Model::name()` returns this so every
    /// Heart event (`agent.turn.done`, `agent.tool.called`, `model_id`
    /// fields) records which configured plug actually answered, not a
    /// generic literal shared by every ACP plug.
    model_name: String,
    /// Set via `Model::set_current_turn_id` right before `chat()` is
    /// called (07d-A5 review #5) — carried into the `AcpPromptMsg` sent to
    /// `session_main`, which hands it to `GateClient` so a gate/tool-call
    /// callback landing on the session's background task can correlate
    /// with the turn actually in flight.
    current_turn_id: Mutex<String>,
    requested_mode: Mutex<Option<String>>,
    native_attachments: Mutex<Vec<bakery_agent::NativeAttachment>>,
    /// The last completed prompt's usage, for `Model::last_turn_usage`.
    last_usage: Mutex<Option<NativeTurnUsage>>,
}

impl AcpPipeModel {
    pub(crate) fn new(
        state: AppState,
        stream_id: String,
        seat: Seat,
        workspace: PathBuf,
        config: AcpSpawnConfig,
        model_name: String,
    ) -> Self {
        Self {
            state,
            stream_id,
            seat,
            workspace,
            binary_override: Some(config),
            revoked: Arc::new(AtomicBool::new(false)),
            owned_generation: Arc::new(Mutex::new(None)),
            model_name,
            current_turn_id: Mutex::new(String::new()),
            requested_mode: Mutex::new(None),
            native_attachments: Mutex::new(Vec::new()),
            last_usage: Mutex::new(None),
        }
    }

    fn live_session(&self) -> Result<LiveAcpSession, String> {
        if self.revoked.load(Ordering::Acquire) {
            return Err("ACP model assignment was revoked — resolve the seat again".to_string());
        }
        let config = self
            .binary_override
            .as_ref()
            .ok_or_else(|| "Setup needed: ACP pipe is not yet enabled".to_string())?;
        let live = ensure_boxed_session(
            &self.state,
            &self.stream_id,
            self.seat,
            &self.model_name,
            &self.workspace,
            config,
        )?;
        *self.owned_generation.lock().unwrap() = Some(live.generation);
        Ok(live)
    }

    fn native_value(value: &NativeConfigValue) -> AcpConfigValue {
        match value {
            NativeConfigValue::Select(value) => AcpConfigValue::Select(value.clone()),
            NativeConfigValue::Boolean(value) => AcpConfigValue::Boolean(*value),
        }
    }

    /// Sends the shown value of every setting whose change timed out, model
    /// first (it can replace the effort menu). The runtime may have applied
    /// that change late without telling Bakery; without this, the picker
    /// could show one model while another runs.
    fn reconfirm_settings(&self, live: &LiveAcpSession) -> Result<(), String> {
        for category in [Some("model"), Some("thought_level"), None] {
            let snapshot = live.snapshot();
            for option in &snapshot.config_options {
                let selected = match category {
                    Some(category) => option.category.as_deref() == Some(category),
                    None => !matches!(option.category.as_deref(), Some("model" | "thought_level")),
                };
                if !selected || !live.shared.lock().unwrap().unconfirmed.contains(&option.id) {
                    continue;
                }
                let value = match &option.current_value {
                    serde_json::Value::String(value) => AcpConfigValue::Select(value.clone()),
                    serde_json::Value::Bool(value) => AcpConfigValue::Boolean(*value),
                    _ => continue,
                };
                set_acp_session_config(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    live.generation,
                    option.id.clone(),
                    value,
                )?;
            }
        }
        Ok(())
    }

    /// Before a prompt: what the picker shows is what runs (a setting whose
    /// change timed out is sent again), and the turn's mode.
    fn apply_requested_controls(&self, live: &LiveAcpSession) -> Result<(), String> {
        self.reconfirm_settings(live)?;
        if let Some(mode) = self.requested_mode.lock().unwrap().clone() {
            self.apply_native_mode(live, &mode)?;
        }
        Ok(())
    }

    fn apply_native_mode(&self, live: &LiveAcpSession, mode: &str) -> Result<(), String> {
        let operator_mode = crate::agent_control::OperatingMode::from_id(mode)
            .ok_or_else(|| format!("unknown Bakery operating mode '{mode}'"))?;
        // The four operating modes are Bakery policy, enforced in the
        // permission callback through `operator_mode` below — not adapter
        // features. So the native session mode is best-effort: when the
        // adapter advertises a callback-keeping mode for this choice, switch to
        // it; when it advertises none (or nothing yet), keep the session where
        // it is. Refusing here used to leave `operator_mode` stale, so Auto
        // never took effect and every Execute gated like Manual.
        let snapshot = live.snapshot();
        if let Some(selected) = native_mode_for(mode, &snapshot.modes) {
            if snapshot.current_mode.as_deref() != Some(selected.id.as_str()) {
                set_acp_session_mode(
                    &self.state,
                    &self.stream_id,
                    self.seat,
                    live.generation,
                    selected.id.clone(),
                )?;
            }
        }
        live.shared.lock().unwrap().operator_mode = Some(operator_mode);
        Ok(())
    }

    fn close_live_generation(&self, live: &LiveAcpSession) {
        live.close();
        remove_acp_slot_generation(
            &self.state,
            &(self.stream_id.clone(), self.seat),
            live.generation,
        );
        bakery_exec::kill_process_group(live.pid);
    }

    fn run_prompt_streaming(
        &self,
        messages: &[ChatMsg],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<bakery_agent::StreamEnd, String> {
        let instruction = messages
            .last()
            .map(|message| message.content.clone())
            .unwrap_or_default();
        // A turn that does not complete must not report the previous one's.
        *self.last_usage.lock().unwrap() = None;
        let live = self.live_session()?;
        let _lock = live
            .prompt_lock
            .try_lock()
            .map_err(|_| "ACP: another prompt is in flight on this seat — retry".to_string())?;
        if self.revoked.load(Ordering::Acquire) || live.is_closed() {
            return Err("ACP session was closed — retry".to_string());
        }
        self.apply_requested_controls(&live)?;

        let attachments = std::mem::take(&mut *self.native_attachments.lock().unwrap());
        let (delta_tx, delta_rx) = std::sync::mpsc::channel::<String>();
        let (reply_tx, mut reply_rx) =
            tokio::sync::oneshot::channel::<Result<AcpPromptReply, String>>();
        let turn_id = self.current_turn_id.lock().unwrap().clone();
        live.prompt_tx
            .blocking_send(AcpPromptMsg {
                text: instruction,
                attachments,
                turn_id: turn_id.clone(),
                delta_tx,
                reply_tx,
            })
            .map_err(|_| "ACP session closed unexpectedly — retry".to_string())?;

        let mut partial = String::new();
        let mut cancellation_sent = false;
        let mut cancellation_started = None;
        let mut deferred_steer_hash: Option<String> = None;
        loop {
            while let Ok(delta) = delta_rx.try_recv() {
                partial.push_str(&delta);
                on_text_delta(&delta);
            }
            match reply_rx.try_recv() {
                Ok(reply) => {
                    while let Ok(delta) = delta_rx.try_recv() {
                        partial.push_str(&delta);
                        on_text_delta(&delta);
                    }
                    let reply = reply?;
                    if cancellation_sent || reply.cancelled {
                        return Ok(bakery_agent::StreamEnd::Stopped {
                            partial_text: partial,
                            reason: bakery_agent::StopReason::Cancelled,
                        });
                    }
                    *self.last_usage.lock().unwrap() = reply.usage;
                    return Ok(bakery_agent::StreamEnd::Completed(
                        bakery_agent::ChatReply::Text(reply.text),
                    ));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    if cancellation_sent {
                        return Ok(bakery_agent::StreamEnd::Stopped {
                            partial_text: partial,
                            reason: bakery_agent::StopReason::Cancelled,
                        });
                    }
                    return Err("ACP session closed before replying — retry".to_string());
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            }

            if cancel.load(Ordering::Acquire) && !cancellation_sent {
                cancellation_sent = true;
                cancellation_started = Some(std::time::Instant::now());
                if cancel_acp_session(&live).is_err() {
                    self.close_live_generation(&live);
                    return Ok(bakery_agent::StreamEnd::Stopped {
                        partial_text: partial,
                        reason: bakery_agent::StopReason::Cancelled,
                    });
                }
            }
            if cancellation_started
                .is_some_and(|started| started.elapsed() > Duration::from_secs(8))
            {
                // The adapter acknowledged the cancel notification but did
                // not settle the original prompt. Fence the generation so
                // queued continuation gets a fresh session and every late
                // callback is ignored.
                self.close_live_generation(&live);
                return Ok(bakery_agent::StreamEnd::Stopped {
                    partial_text: partial,
                    reason: bakery_agent::StopReason::Cancelled,
                });
            }

            if !cancellation_sent {
                // Claimed while it goes in, so the operator can no longer
                // take it back once the runtime may have it.
                if let Some((steer, _claim)) =
                    crate::agent_control::claim_next_steer(&self.state, &self.stream_id, self.seat)
                {
                    if deferred_steer_hash.as_deref() != Some(steer.event_hash.as_str()) {
                        let agent_id = crate::node::agent_id(&self.stream_id, self.seat);
                        let outcome = crate::attachments::native_attachments_for_turn(
                            &self.state,
                            &self.stream_id,
                            &agent_id,
                            &steer.attachment_ids,
                        )
                        .and_then(|attachments| {
                            steer_acp_session(&live, steer.instruction.clone(), attachments)
                        });
                        match outcome {
                            Ok(AcpSteerOutcome::Injected) => {
                                crate::agent_control::mark_steer_applied(
                                    &self.state,
                                    &self.stream_id,
                                    self.seat,
                                    &turn_id,
                                    &steer,
                                )?;
                                deferred_steer_hash = None;
                            }
                            Ok(AcpSteerOutcome::PromptRequired { .. }) | Err(_) => {
                                // Retain the durable queue event. The normal
                                // dispatcher will deliver it exactly once as
                                // the next prompt after this turn settles —
                                // and report an attachment it cannot send.
                                deferred_steer_hash = Some(steer.event_hash);
                            }
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for AcpPipeModel {
    fn drop(&mut self) {
        self.revoked.store(true, Ordering::Release);
        let Some(generation) = *self.owned_generation.lock().unwrap() else {
            return;
        };
        let key = (self.stream_id.clone(), self.seat);
        let pid = {
            let mut sessions = self.state.acp_sessions.lock().unwrap();
            match sessions.get(&key) {
                Some(AcpSlot::Live(live)) if live.generation == generation => {
                    let pid = live.pid;
                    live.close();
                    sessions.insert(key, AcpSlot::Closing { generation });
                    Some(pid)
                }
                Some(AcpSlot::Spawning {
                    generation: current,
                    closed,
                }) if *current == generation => {
                    closed.store(true, Ordering::Release);
                    sessions.insert(key, AcpSlot::Closing { generation });
                    None
                }
                _ => None,
            }
        };
        if let Some(pid) = pid {
            bakery_exec::kill_process_group(pid);
        }
    }
}

impl Model for AcpPipeModel {
    fn name(&self) -> &str {
        &self.model_name
    }

    fn transport(&self) -> bakery_agent::Transport {
        bakery_agent::Transport::Acp
    }

    fn supports_chat(&self) -> bool {
        true
    }

    fn chat(&self, _system: &str, messages: &[ChatMsg]) -> Result<String, String> {
        let cancel = AtomicBool::new(false);
        match self.run_prompt_streaming(messages, &mut |_| {}, &cancel)? {
            bakery_agent::StreamEnd::Completed(bakery_agent::ChatReply::Text(text)) => Ok(text),
            bakery_agent::StreamEnd::Stopped { reason, .. } => {
                Err(format!("ACP prompt stopped: {reason:?}"))
            }
            bakery_agent::StreamEnd::Completed(_) => {
                Err("ACP native runtime returned an unexpected tool-call envelope".to_string())
            }
        }
    }

    fn chat_turn_streaming(
        &self,
        _system: &str,
        messages: &[ChatMsg],
        _tools: &[bakery_agent::ToolDecl],
        on_text_delta: &mut dyn FnMut(&str),
        cancel: &AtomicBool,
    ) -> Result<bakery_agent::StreamEnd, String> {
        self.run_prompt_streaming(messages, on_text_delta, cancel)
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn last_turn_usage(&self) -> Option<bakery_agent::TurnUsage> {
        self.last_usage.lock().unwrap().as_ref().map(|(tokens, _)| *tokens)
    }

    fn last_turn_usage_detail(&self) -> Option<bakery_agent::TurnUsageDetail> {
        self.last_usage.lock().unwrap().as_ref().map(|(_, detail)| detail.clone())
    }

    fn propose_edit(&self, _req: &EditRequest) -> Result<ProposedEdit, String> {
        Err("acp: direct file-path edits are not supported by the piped plug".to_string())
    }

    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }

    fn set_current_turn_id(&self, turn_id: &str) {
        *self.current_turn_id.lock().unwrap() = turn_id.to_string();
    }

    fn ensure_runtime_ready(&self) -> Result<(), String> {
        self.live_session().map(|_| ())
    }

    fn set_native_operating_mode(&self, mode: Option<&str>) -> Result<(), String> {
        *self.requested_mode.lock().unwrap() = mode.map(str::to_string);
        let Some(mode) = mode else { return Ok(()) };
        // The mode is already stored above, and `apply_requested_controls`
        // applies `requested_mode` at the next dispatch, before its prompt. So
        // switching between turns — when no ACP session is live yet — is not
        // a failure: it sticks and takes effect on the next turn. Only push
        // into a session that is live right now, so a mid-turn switch is seen
        // by the very next permission callback; never start one for it, or a
        // turn starts its runtime twice (and resumes its session twice). This
        // is what makes the mode per-agent and switchable mid-chat without a
        // spurious error that leaves the composer showing a mode the session
        // never accepted.
        match live_acp_session(&self.state, &self.stream_id, self.seat) {
            Some(live) if !self.revoked.load(Ordering::Acquire) => self.apply_native_mode(&live, mode),
            _ => Ok(()),
        }
    }

    fn set_native_attachments(
        &self,
        attachments: Vec<bakery_agent::NativeAttachment>,
    ) -> Result<(), String> {
        // Nothing to validate: every attachment converts, against whatever
        // the session advertises (see `native_attachment_blocks`).
        *self.native_attachments.lock().unwrap() = attachments;
        Ok(())
    }

}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::project::open_stream;
    use crate::testutil::{
        bakery_node_binary, fake_acp_agent_binary, make_repo, open_req, state_with,
    };
    use bakery_agent::StubModel;
    use futures::AsyncReadExt;
    use libc;

    /// Only a Bakery tool on Bakery's own MCP server skips the runtime-level
    /// card (its own gate asks where it runs) — never another server's tool
    /// of the same name, a bare name, or a name Bakery does not have.
    #[test]
    fn only_bakery_tool_calls_skip_the_runtime_card() {
        assert!(is_bakery_tool_call("mcp__bakery__thread_list"));
        assert!(is_bakery_tool_call("mcp__bakery__thread_create"));
        assert!(is_bakery_tool_call("mcp__bakery__api_call"));
        assert!(!is_bakery_tool_call("mcp__bakery__auth_api_key"));
        assert!(!is_bakery_tool_call("mcp__other__thread_list"));
        assert!(!is_bakery_tool_call("thread_list"));
        assert!(!is_bakery_tool_call("mcp__bakery__"));
    }

    /// Each runtime's own mode names (from its adapter source or docs) map to
    /// the mode where it still asks before acting — never Cursor's or Grok
    /// Build's read-only `ask`, which made every Bakery mode unable to edit.
    #[test]
    fn every_runtime_gets_a_native_mode_that_still_asks() {
        let pick = |mode: &str, ids: &[&str]| {
            let offered: Vec<AcpMode> = ids
                .iter()
                .map(|id| AcpMode { id: id.to_string(), name: id.to_string(), description: None })
                .collect();
            native_mode_for(mode, &offered).map(|m| m.id.clone())
        };
        let claude = ["default", "acceptEdits", "plan", "bypassPermissions"];
        let cursor = ["agent", "plan", "ask"];
        let codex = ["read-only", "auto", "full-access"];
        let grok_build = ["default", "plan", "ask"];
        let hermes = ["default", "accept_edits", "dont_ask"];
        for mode in ["manual", "auto_edit", "auto"] {
            assert_eq!(pick(mode, &claude).as_deref(), Some("default"));
            assert_eq!(pick(mode, &cursor).as_deref(), Some("agent"), "{mode}");
            assert_eq!(pick(mode, &codex).as_deref(), Some("read-only"), "{mode}");
            assert_eq!(pick(mode, &grok_build).as_deref(), Some("default"));
            assert_eq!(pick(mode, &hermes).as_deref(), Some("default"));
        }
        assert_eq!(pick("plan", &claude).as_deref(), Some("plan"));
        assert_eq!(pick("plan", &cursor).as_deref(), Some("plan"));
        assert_eq!(pick("plan", &codex).as_deref(), Some("read-only"));
        assert_eq!(pick("plan", &grok_build).as_deref(), Some("plan"));
        // No read-only mode of its own: the session stays put and Bakery's
        // permission callback alone holds Plan.
        assert_eq!(pick("plan", &hermes), None);
    }

    /// A screenshot a native runtime echoes back reaches neither Heart nor
    /// the side store, as an image block or as that block in JSON text.
    #[test]
    fn a_native_tool_events_screenshot_pixels_are_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let png = format!("iVBORw0KGgo{}", "A".repeat(MAX_ACP_TOOL_EVENT_STRING * 3));
        let echoed = serde_json::json!([{"type": "image", "data": png}, {"type": "text", "text": "{\"width\":2}"}]);
        record_acp_tool_event(
            &state,
            "socket_shot",
            Seat::Baker,
            "claude-native",
            "turn_1",
            "tool_call_update",
            &serde_json::json!({
                "toolCallId": "toolu_1",
                "content": [{"type": "content", "content": {"type": "image", "data": png, "mimeType": "image/png"}}],
                "rawOutput": echoed.to_string(),
            }),
        );
        let events = state.heart.lock().unwrap().read_stream("socket_shot").unwrap();
        let payload = &events[0].payload;
        assert!(!payload.contains("iVBORw0KGgo") && !payload.contains("AAAA"), "{payload}");
        assert!(payload.contains("(image not kept)") && payload.contains("width"), "{payload}");
        let p: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert!(p.get("full").is_none(), "nothing big is left to keep beside Heart: {p}");
    }

    /// Every character of a native tool call stays reachable: Heart keeps the
    /// bounded copy plus a `full` reference, and the side store returns the
    /// exact original — only for its own stream, and only while intact.
    #[test]
    fn a_truncated_tool_event_keeps_its_complete_payload_beside_heart() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let big = "x".repeat(MAX_ACP_TOOL_EVENT_STRING * 3);
        record_acp_tool_event(
            &state,
            "socket_payload",
            Seat::Baker,
            "claude-native",
            "turn_1",
            "tool_call_update",
            &serde_json::json!({
                "toolCallId": "toolu_1",
                "title": "Read big.txt",
                "rawOutput": { "text": big },
            }),
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream("socket_payload")
            .unwrap();
        let p: serde_json::Value = serde_json::from_str(&events[0].payload).unwrap();
        assert!(p["detail"]["rawOutput"]["text"]
            .as_str()
            .unwrap()
            .ends_with("… [truncated]"));
        let sha = p["full"]["sha"]
            .as_str()
            .expect("full reference")
            .to_string();

        let full = read_full_tool_payload(&state, "socket_payload", &sha).unwrap();
        let back: serde_json::Value = serde_json::from_str(&full).unwrap();
        assert_eq!(back["rawOutput"]["text"].as_str().unwrap(), big);

        assert!(read_full_tool_payload(&state, "socket_other", &sha).is_err());
        assert!(read_full_tool_payload(&state, "socket_payload", "../../etc/passwd").is_err());
        let path = tool_payload_dir(&state, "socket_payload").join(format!("{sha}.json"));
        std::fs::write(&path, "{}").unwrap();
        assert!(read_full_tool_payload(&state, "socket_payload", &sha)
            .unwrap_err()
            .contains("integrity"));

        // A small event is its own whole record — no side copy.
        record_acp_tool_event(
            &state,
            "socket_payload",
            Seat::Baker,
            "claude-native",
            "turn_1",
            "tool_call",
            &serde_json::json!({ "toolCallId": "toolu_2", "title": "ls" }),
        );
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream("socket_payload")
            .unwrap();
        let p2: serde_json::Value = serde_json::from_str(&events[1].payload).unwrap();
        assert!(p2.get("full").is_none(), "{p2}");
    }

    #[test]
    fn native_tool_running_phase_maps_title_call_id_and_kind() {
        let phase = native_tool_running_phase(
            "Edit target.txt",
            "toolu_abc",
            "2026-09-23T00:00:00Z".to_string(),
            Some("target.txt".to_string()),
        );
        match phase {
            crate::state::ActivityPhase::RunningTool {
                tool,
                call_id,
                started_at,
                kind,
                preview,
                output,
            } => {
                assert_eq!(preview.as_deref(), Some("target.txt"));
                assert_eq!(output, None, "no output before the call has run");
                assert_eq!(tool, "Edit target.txt");
                assert_eq!(call_id, "toolu_abc");
                assert_eq!(started_at, "2026-09-23T00:00:00Z");
                assert!(matches!(kind, crate::state::ToolRunKind::Tool));
            }
            other => panic!("expected RunningTool, got {other:?}"),
        }
    }

    /// The activity phase set on a native `ToolCall` is fenced to the current
    /// turn: a stale turn id must not clobber a newer turn's phase. This locks
    /// in the `still_this_turn` guard the ToolCall arm relies on.
    #[test]
    fn a_stale_native_tool_call_does_not_clobber_a_newer_turns_phase() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), std::sync::Arc::new(StubModel));
        let stream_id = "wt:native-activity";
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        // A newer turn owns the activity, mid-stream.
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn_new");
        crate::state::set_agent_phase(
            &state,
            &agent_id,
            crate::state::ActivityPhase::Streaming { text: "hi".into() },
        );
        // Simulate the guard the ToolCall arm applies for a STALE turn id.
        let stale_turn = "turn_old";
        let still_this_turn = crate::state::agent_activity_snapshot(&state, &agent_id)
            .is_some_and(|a| a.turn_id == stale_turn && !a.cancel_requested);
        assert!(!still_this_turn, "a stale turn must fail the guard");
        if still_this_turn {
            crate::state::set_agent_phase(
                &state,
                &agent_id,
                native_tool_running_phase("Edit", "x", "t".into(), None),
            );
        }
        // The newer turn's Streaming phase is intact.
        assert!(matches!(
            crate::state::agent_activity_snapshot(&state, &agent_id)
                .unwrap()
                .phase,
            crate::state::ActivityPhase::Streaming { .. }
        ));
    }

    fn host_rustc_short_version() -> String {
        let out = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .nth(1)
            .unwrap()
            .to_string()
    }

    fn fake_model(
        state: &AppState,
        stream_id: &str,
        extra_env: Vec<(String, String)>,
    ) -> AcpPipeModel {
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = Box::leak(Box::new(workspace)).path().to_path_buf();
        fake_model_in_workspace(state, stream_id, workspace_path, extra_env, false)
    }

    /// 07b proof: a workspace with **no** `mise.toml` at all — the thin room
    /// (`ensure_thin_room`) must still boot the ACP plug. No toolchain seed,
    /// no `mise.toml` write — this is the "no pack" case, not a variant of
    /// the pinned-toolchain fixture above.
    fn fake_model_in_thin_workspace(
        state: &AppState,
        stream_id: &str,
        workspace_path: PathBuf,
    ) -> AcpPipeModel {
        AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace_path,
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![],
                trusted: false,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        )
    }

    fn fake_model_in_workspace(
        state: &AppState,
        stream_id: &str,
        workspace_path: PathBuf,
        extra_env: Vec<(String, String)>,
        trusted: bool,
    ) -> AcpPipeModel {
        std::fs::write(
            workspace_path.join("mise.toml"),
            format!("[tools]\nrust = \"{}\"\n", host_rustc_short_version()),
        )
        .unwrap();
        crate::testutil::seed_test_toolchain_cache(&state.base.join("cache"), &workspace_path);
        AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace_path,
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env,
                trusted,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        )
    }

    fn pending_gate_events(state: &AppState, stream_id: &str) -> Vec<serde_json::Value> {
        state
            .heart
            .lock()
            .unwrap()
            .read_stream(stream_id)
            .unwrap()
            .iter()
            .filter(|e| e.family == CHILD_TOOL_PENDING_FAMILY)
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect()
    }

    fn acp_tool_call_events(state: &AppState, stream_id: &str) -> Vec<serde_json::Value> {
        state
            .heart
            .lock()
            .unwrap()
            .read_stream(stream_id)
            .unwrap()
            .iter()
            .filter(|e| e.family == ACP_TOOL_EVENT_FAMILY)
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect()
    }

    fn wait_for_acp_pid(state: &AppState, stream_id: &str) -> u32 {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = state
                .lifecycle
                .lock()
                .unwrap()
                .activity
                .get(stream_id)
                .and_then(|a| a.acp_pids.first().copied())
            {
                return pid;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ACP child for {stream_id} never reached the live-pid registry"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn assert_pid_reaped(pid: u32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
            if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ACP pid {pid} still exists after the bounded reap window"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_acp_registry_clear(state: &AppState, stream_id: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let clear = state
                .lifecycle
                .lock()
                .unwrap()
                .activity
                .get(stream_id)
                .map(|a| a.acp_pending == 0 && a.acp_pids.is_empty())
                .unwrap_or(true);
            if clear {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ACP lifecycle registry for {stream_id} did not clear after reap"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_for_runtime(
        state: &AppState,
        stream_id: &str,
        ready: impl Fn(&AcpRuntimeSnapshot) -> bool,
    ) -> AcpRuntimeSnapshot {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(snapshot) = acp_runtime_snapshot(state, stream_id, Seat::Orchestrator) {
                if ready(&snapshot) {
                    return snapshot;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "ACP capability snapshot for {stream_id} never became ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    // ─── ACP round-trip via the fake agent ───────────────────────────────────

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn round_trip_returns_the_fake_agents_text_raw_not_json_wrapped() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(&state, "wt:piperound", Vec::new());

        // 07d-A3B: `chat()` must not fake the 8-step JSON tool-loop's
        // `{"done":{"summary":...}}` envelope — the caller
        // (`routes::agent::agent_acp_turn`) records this text directly.
        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(reply, "hello from fake agent");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_session_retains_advertised_controls_and_acknowledges_dependent_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-capabilities";
        let model = fake_model(
            &state,
            stream_id,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                (
                    "BAKERY_FAKE_ACP_CONTEXT_OPTION".to_string(),
                    "1".to_string(),
                ),
            ],
        );
        model.ensure_runtime_ready().unwrap();

        let initial = wait_for_runtime(&state, stream_id, |snapshot| {
            !snapshot.commands.is_empty()
                && snapshot.usage.is_some()
                && snapshot.auth_status.is_some()
        });
        assert_eq!(
            initial.runtime.as_ref().unwrap().title.as_deref(),
            Some("Bakery Fake Native Agent")
        );
        assert!(
            initial.steering,
            "top-level initialize _meta must advertise steering"
        );
        assert!(initial.prompt.image && initial.prompt.audio && initial.prompt.embedded_context);
        assert_eq!(initial.auth_methods[0].kind, "terminal");
        assert_eq!(initial.commands[0].name, "native-review");
        assert_eq!(
            initial.usage.as_ref().map(|usage| usage.size),
            Some(1_000_000)
        );
        assert_eq!(
            initial
                .auth_status
                .as_ref()
                .and_then(|status| status.plan.as_deref()),
            Some("max")
        );
        assert_eq!(
            initial
                .config_options
                .iter()
                .find(|option| option.category.as_deref() == Some("context"))
                .unwrap()
                .current_value,
            serde_json::json!("1m")
        );
        let serialized = serde_json::to_string(&initial).unwrap();
        assert!(
            !serialized.contains("must-not-cross"),
            "account identity must be stripped at the adapter boundary"
        );

        let changed = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            initial.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap();
        assert_eq!(changed.requested_model.as_deref(), Some("fake-sonnet"));
        assert_eq!(changed.effective_model.as_deref(), Some("fake-sonnet"));
        let effort = changed
            .config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("thought_level"))
            .unwrap();
        assert_eq!(effort.current_value, serde_json::json!("high"));
        assert_eq!(
            effort
                .values
                .iter()
                .map(|value| value.id.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "high"],
            "the runtime's complete acknowledgement must replace incompatible dependent values"
        );
        let context = changed
            .config_options
            .iter()
            .find(|option| option.category.as_deref() == Some("context"))
            .unwrap();
        assert_eq!(context.current_value, serde_json::json!("128k"));
        assert_eq!(
            context
                .values
                .iter()
                .map(|value| value.id.as_str())
                .collect::<Vec<_>>(),
            vec!["128k"],
            "a model acknowledgement also replaces its dependent context options"
        );

        let stale = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            initial.generation + 1,
            "model".to_string(),
            AcpConfigValue::Select("fake-opus".to_string()),
        )
        .unwrap_err();
        assert!(stale.contains("stale ACP session generation"), "{stale}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn acknowledged_native_configuration_changes_the_next_prompt_and_a_rejection_keeps_the_last_value(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-config-execution";
        let model = fake_model(
            &state,
            stream_id,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_REPORT_CONFIG".to_string(), "1".to_string()),
                (
                    "BAKERY_FAKE_ACP_CONTEXT_OPTION".to_string(),
                    "1".to_string(),
                ),
                (
                    "BAKERY_FAKE_ACP_REJECT_CONFIG_VALUE".to_string(),
                    "fake-opus".to_string(),
                ),
            ],
        );
        model.ensure_runtime_ready().unwrap();
        let initial = wait_for_runtime(&state, stream_id, |_| true);

        let after_model = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            initial.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap();
        let after_effort = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            after_model.generation,
            "effort".to_string(),
            AcpConfigValue::Select("low".to_string()),
        )
        .unwrap();
        let after_contextish_toggle = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            after_effort.generation,
            "fast".to_string(),
            AcpConfigValue::Boolean(true),
        )
        .unwrap();

        let rejected = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            after_contextish_toggle.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-opus".to_string()),
        )
        .unwrap_err();
        assert!(rejected.contains("configuration rejected"), "{rejected}");
        let acknowledged = acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator).unwrap();
        assert_eq!(acknowledged.effective_model.as_deref(), Some("fake-sonnet"));
        assert_eq!(
            acknowledged
                .config_options
                .iter()
                .find(|option| option.id == "effort")
                .unwrap()
                .current_value,
            serde_json::json!("low")
        );
        assert_eq!(
            acknowledged
                .config_options
                .iter()
                .find(|option| option.id == "context")
                .unwrap()
                .current_value,
            serde_json::json!("128k")
        );

        let reply = model
            .chat("system", &[ChatMsg::user("prove the effective controls")])
            .unwrap();
        assert!(reply.contains("\"model\":\"fake-sonnet\""), "{reply}");
        assert!(reply.contains("\"effort\":\"low\""), "{reply}");
        assert!(reply.contains("\"fast\":true"), "{reply}");
        assert!(reply.contains("\"context\":\"128k\""), "{reply}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_setting_the_runtime_answers_too_late_is_sent_again_before_the_next_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-config-late";
        let late_ms = (ACP_CONFIG_TIMEOUT + Duration::from_secs(1)).as_millis();
        let model = fake_model(
            &state,
            stream_id,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_REPORT_CONFIG".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_LATE_CONFIG_MS".to_string(), late_ms.to_string()),
            ],
        );
        model.ensure_runtime_ready().unwrap();
        let initial = wait_for_runtime(&state, stream_id, |_| true);
        assert_eq!(initial.effective_model.as_deref(), Some("fake-opus"));

        // The runtime applies the change, but its answer comes too late.
        let error = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            initial.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap_err();
        assert!(error.starts_with(CONFIG_UNANSWERED), "{error}");
        let shown = acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator).unwrap();
        assert_eq!(shown.effective_model.as_deref(), Some("fake-opus"));

        // The next prompt runs on the model Bakery shows, not the late one.
        let reply = model
            .chat("system", &[ChatMsg::user("which model runs?")])
            .unwrap();
        assert!(reply.contains("\"model\":\"fake-opus\""), "{reply}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_config_acknowledgements_restore_on_a_fresh_generation_for_the_same_agent() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-config-recovery";
        let env = vec![
            ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
            ("BAKERY_FAKE_ACP_REPORT_CONFIG".to_string(), "1".to_string()),
            (
                "BAKERY_FAKE_ACP_CONTEXT_OPTION".to_string(),
                "1".to_string(),
            ),
        ];
        let model = fake_model(&state, stream_id, env.clone());
        model.ensure_runtime_ready().unwrap();
        let initial = wait_for_runtime(&state, stream_id, |_| true);
        let selected = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            initial.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap();
        let selected = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            selected.generation,
            "effort".to_string(),
            AcpConfigValue::Select("low".to_string()),
        )
        .unwrap();
        let selected = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            selected.generation,
            "fast".to_string(),
            AcpConfigValue::Boolean(true),
        )
        .unwrap();
        let selected = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            selected.generation,
            "context".to_string(),
            AcpConfigValue::Select("128k".to_string()),
        )
        .unwrap();
        for (config_id, effective) in [
            ("model", serde_json::json!("fake-sonnet")),
            ("effort", serde_json::json!("low")),
            ("fast", serde_json::json!(true)),
            ("context", serde_json::json!("128k")),
        ] {
            crate::state::append(
                &state,
                stream_id,
                "human",
                "agent.native_config.acknowledged",
                serde_json::json!({
                    "agent_id": crate::node::agent_id(stream_id, Seat::Orchestrator),
                    "seat": Seat::Orchestrator,
                    "account_id": &selected.account_id,
                    "generation": selected.generation,
                    "config_id": config_id,
                    "requested": effective.clone(),
                    "effective": effective,
                    "provenance": selected.provenance,
                })
                .to_string(),
                vec![],
            )
            .unwrap();
        }

        drop(model);
        let key = (stream_id.to_string(), Seat::Orchestrator);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.acp_sessions.lock().unwrap().contains_key(&key) {
            assert!(
                std::time::Instant::now() < deadline,
                "old ACP generation did not close"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        let recovered = fake_model(&state, stream_id, env);
        recovered.ensure_runtime_ready().unwrap();
        let snapshot = wait_for_runtime(&state, stream_id, |snapshot| {
            snapshot.effective_model.as_deref() == Some("fake-sonnet")
        });
        assert_ne!(snapshot.generation, initial.generation);
        assert_eq!(
            snapshot
                .config_options
                .iter()
                .find(|option| option.id == "effort")
                .unwrap()
                .current_value,
            serde_json::json!("low")
        );
        assert_eq!(
            snapshot
                .config_options
                .iter()
                .find(|option| option.id == "fast")
                .unwrap()
                .current_value,
            serde_json::json!(true)
        );
        assert_eq!(
            snapshot
                .config_options
                .iter()
                .find(|option| option.id == "context")
                .unwrap()
                .current_value,
            serde_json::json!("128k")
        );
        let reply = recovered
            .chat("system", &[ChatMsg::user("prove recovered configuration")])
            .unwrap();
        assert!(reply.contains("\"model\":\"fake-sonnet\""), "{reply}");
        assert!(reply.contains("\"effort\":\"low\""), "{reply}");
        assert!(reply.contains("\"fast\":true"), "{reply}");
        assert!(reply.contains("\"context\":\"128k\""), "{reply}");
    }

    /// A model and effort chosen with no session yet (thread_create's, saved
    /// by kind) are what the agent's first session starts on and its picker
    /// shows, and what a session after a restart comes back on.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_choice_made_before_any_session_is_what_its_sessions_run_on() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-chosen-by-kind";
        let env = vec![
            ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
            ("BAKERY_FAKE_ACP_REPORT_CONFIG".to_string(), "1".to_string()),
        ];
        // The fake starts on fake-opus at max.
        for (category, value) in [(crate::selection::MODEL, "fake-sonnet"), (crate::selection::EFFORT, "low")] {
            crate::selection::choose(&state, stream_id, Seat::Orchestrator, "acp", category, Some(value), None, "human").unwrap();
        }
        let model = fake_model(&state, stream_id, env.clone());
        let reply = model.chat("system", &[ChatMsg::user("work as opened")]).unwrap();
        assert!(reply.contains("\"model\":\"fake-sonnet\""), "{reply}");
        assert!(reply.contains("\"effort\":\"low\""), "{reply}");
        let shown = acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator).unwrap();
        let effort = shown.config_options.iter().find(|option| option.id == "effort").unwrap();
        assert_eq!(effort.current_value, serde_json::json!("low"), "the picker shows what runs");

        drop(model);
        let key = (stream_id.to_string(), Seat::Orchestrator);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.acp_sessions.lock().unwrap().contains_key(&key) {
            assert!(std::time::Instant::now() < deadline, "old ACP generation did not close");
            std::thread::sleep(Duration::from_millis(10));
        }
        let recovered = fake_model(&state, stream_id, env);
        let reply = recovered.chat("system", &[ChatMsg::user("carry on")]).unwrap();
        assert_ne!(acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator).unwrap().generation, shown.generation);
        assert!(reply.contains("\"model\":\"fake-sonnet\""), "{reply}");
        assert!(reply.contains("\"effort\":\"low\""), "{reply}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn stale_native_generation_cannot_overwrite_a_replaced_session_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-config-stale-replacement";
        let env = vec![("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string())];
        let original = fake_model(&state, stream_id, env.clone());
        original.ensure_runtime_ready().unwrap();
        let old_generation = wait_for_runtime(&state, stream_id, |_| true).generation;

        // Make the next operation use a genuinely new ACP generation, as a
        // respawn/recovery does; a delayed picker request still carries the
        // old generation and must be unable to mutate it.
        drop(original);
        let key = (stream_id.to_string(), Seat::Orchestrator);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state.acp_sessions.lock().unwrap().contains_key(&key) {
            assert!(
                std::time::Instant::now() < deadline,
                "old ACP generation did not close"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let replacement = fake_model(&state, stream_id, env);
        replacement.ensure_runtime_ready().unwrap();
        let fresh = wait_for_runtime(&state, stream_id, |_| true);
        assert_ne!(fresh.generation, old_generation);
        let acknowledged = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            fresh.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap();
        assert_eq!(acknowledged.effective_model.as_deref(), Some("fake-sonnet"));

        let stale = set_acp_session_config(
            &state,
            stream_id,
            Seat::Orchestrator,
            old_generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-opus".to_string()),
        )
        .unwrap_err();
        assert!(stale.contains("stale ACP session generation"), "{stale}");
        assert_eq!(
            acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator)
                .unwrap()
                .effective_model
                .as_deref(),
            Some("fake-sonnet"),
            "a delayed old-session request must not overwrite the fresh acknowledgement"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_configuration_is_isolated_per_agent_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let env = vec![("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string())];
        let first = fake_model(&state, "wt:native-config-a", env.clone());
        let second = fake_model(&state, "wt:native-config-b", env);
        first.ensure_runtime_ready().unwrap();
        second.ensure_runtime_ready().unwrap();
        let first_snapshot = wait_for_runtime(&state, "wt:native-config-a", |_| true);
        let second_snapshot = wait_for_runtime(&state, "wt:native-config-b", |_| true);

        set_acp_session_config(
            &state,
            "wt:native-config-a",
            Seat::Orchestrator,
            first_snapshot.generation,
            "model".to_string(),
            AcpConfigValue::Select("fake-sonnet".to_string()),
        )
        .unwrap();

        assert_eq!(
            acp_runtime_snapshot(&state, "wt:native-config-a", Seat::Orchestrator)
                .unwrap()
                .effective_model
                .as_deref(),
            Some("fake-sonnet")
        );
        assert_eq!(
            acp_runtime_snapshot(&state, "wt:native-config-b", Seat::Orchestrator)
                .unwrap()
                .effective_model
                .as_deref(),
            Some("fake-opus"),
            "one agent changing model must not mutate another session on the same account"
        );
        assert_ne!(first_snapshot.generation, second_snapshot.generation);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn cancellation_settles_the_prompt_and_the_same_session_continues() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-cancel-continue";
        let model = Arc::new(fake_model(
            &state,
            stream_id,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                (
                    "BAKERY_FAKE_ACP_HOLD_PROMPT_ONCE".to_string(),
                    "1".to_string(),
                ),
            ],
        ));
        model.ensure_runtime_ready().unwrap();
        let generation = wait_for_runtime(&state, stream_id, |_| true).generation;
        let cancel = Arc::new(AtomicBool::new(false));
        let running = {
            let model = model.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let mut deltas = String::new();
                let end = model
                    .chat_turn_streaming(
                        "system",
                        &[ChatMsg::user("answer, then wait")],
                        &[],
                        &mut |delta| deltas.push_str(delta),
                        &cancel,
                    )
                    .unwrap();
                (end, deltas)
            })
        };
        std::thread::sleep(Duration::from_millis(120));
        cancel.store(true, Ordering::Release);
        let (end, deltas) = running.join().unwrap();
        assert!(matches!(
            end,
            bakery_agent::StreamEnd::Stopped {
                reason: bakery_agent::StopReason::Cancelled,
                ..
            }
        ));
        assert!(deltas.contains("working before cancellation"));

        let continued = model
            .chat("system", &[ChatMsg::user("continue once")])
            .unwrap();
        assert_eq!(continued, "hello from fake agent");
        assert_eq!(
            acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator)
                .unwrap()
                .generation,
            generation,
            "a cooperative Stop must keep the native session available for continuation"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn durable_native_steer_is_injected_and_consumed_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-steer"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        state.queue_dispatchers.lock().unwrap().insert(agent_id);
        let model = Arc::new(fake_model(
            &state,
            &stream_id,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_HOLD_PROMPT".to_string(), "1".to_string()),
                (
                    "BAKERY_FAKE_ACP_STEER_COMPLETES".to_string(),
                    "1".to_string(),
                ),
                (
                    "BAKERY_FAKE_ACP_REPORT_PROMPT_BLOCKS".to_string(),
                    "1".to_string(),
                ),
            ],
        ));
        model.set_current_turn_id("native-turn-1");
        let upload = serde_json::from_value(serde_json::json!({
            "stream_id": stream_id,
            "seat": "orchestrator",
            "filename": "notes.txt",
            "media_type": "text/plain",
            "data_url": "data:text/plain;base64,cHJpdmF0ZSBub3Rl",
        }))
        .unwrap();
        let attachment = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(crate::attachments::upload_attachment(
                axum::extract::State(state.clone()),
                axum::Json(upload),
            ))
            .unwrap()
            .0;

        // Establish a genuinely active prompt before queueing the steer. A
        // pre-queued steer belongs to the normal follow-up dispatcher; it is
        // not evidence that an in-flight native prompt can be steered. The
        // first streamed chunk is the fake adapter's acknowledgement that it
        // is holding `session/prompt` open and can now accept steering.
        let cancel = Arc::new(AtomicBool::new(false));
        let (delta_tx, delta_rx) = std::sync::mpsc::channel::<String>();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let running = {
            let model = model.clone();
            let cancel = cancel.clone();
            std::thread::spawn(move || {
                let mut deltas = String::new();
                let result = model.chat_turn_streaming(
                    "system",
                    &[ChatMsg::user("begin work")],
                    &[],
                    &mut |delta| {
                        deltas.push_str(delta);
                        let _ = delta_tx.send(delta.to_string());
                    },
                    &cancel,
                );
                let _ = done_tx.send((result, deltas));
            })
        };
        let first_delta = match delta_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(delta) => delta,
            Err(error) => {
                cancel.store(true, Ordering::Release);
                let _ = done_rx.recv_timeout(Duration::from_secs(5));
                let _ = running.join();
                panic!("native prompt did not become steerable in time: {error}");
            }
        };
        assert!(
            first_delta.contains("working before cancellation"),
            "{first_delta}"
        );

        let queued = crate::agent_control::enqueue(
            &state,
            crate::routes::agent::AgentTurnReq {
                stream_id: stream_id.clone(),
                instruction: "Explain only; make no edits".to_string(),
                seat: Some(Seat::Orchestrator),
                ..Default::default()
            },
            vec![attachment.id],
            true,
        )
        .unwrap();
        let (end, deltas) = match done_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result,
            Err(error) => {
                cancel.store(true, Ordering::Release);
                let _ = done_rx.recv_timeout(Duration::from_secs(5));
                let _ = running.join();
                panic!("native steer did not settle the prompt in time: {error}");
            }
        };
        running.join().unwrap();
        let reply = match end.unwrap() {
            bakery_agent::StreamEnd::Completed(bakery_agent::ChatReply::Text(reply)) => reply,
            other => {
                panic!("native steer did not complete the prompt: {other:?}; deltas: {deltas}")
            }
        };
        // The steer reaches the adapter as a prompt would: its text, then its
        // attachment, resolved exactly as a prompt's (a text file, inline).
        assert!(
            reply.contains(r#""text":"Explain only; make no edits""#)
                && reply.contains(crate::attachments::ATTACHED_PREAMBLE),
            "{reply}"
        );
        assert!(
            crate::agent_control::peek_next_steer(&state, &stream_id, Seat::Orchestrator).is_none()
        );
        let applied = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&stream_id)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.family == crate::agent_control::STEER_APPLIED
                    && event.causality.contains(&queued.event_hash)
            })
            .count();
        assert_eq!(applied, 1);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_attachments_follow_only_the_advertised_prompt_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(
            &state,
            "wt:native-attachments",
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                (
                    "BAKERY_FAKE_ACP_REPORT_PROMPT_BLOCKS".to_string(),
                    "1".to_string(),
                ),
            ],
        );
        model
            .set_native_attachments(vec![
                bakery_agent::NativeAttachment {
                    filename: "notes.txt".to_string(),
                    mime_type: "text/plain".to_string(),
                    data: b"bounded context".to_vec(),
                },
                bakery_agent::NativeAttachment {
                    filename: "pixel.png".to_string(),
                    mime_type: "image/png".to_string(),
                    data: b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR\0\0\0\x01\0\0\0\x01".to_vec(),
                },
                bakery_agent::NativeAttachment {
                    filename: "sample.wav".to_string(),
                    mime_type: "audio/wav".to_string(),
                    data: vec![4, 5, 6],
                },
                // Neither goes as itself: the Claude adapter drops embedded
                // binaries, and the API takes no HEIC. Both are named.
                bakery_agent::NativeAttachment {
                    filename: "spec.pdf".to_string(),
                    mime_type: "application/pdf".to_string(),
                    data: b"%PDF\x00".to_vec(),
                },
                bakery_agent::NativeAttachment {
                    filename: "photo.heic".to_string(),
                    mime_type: "image/heic".to_string(),
                    data: vec![0, 1, 2],
                },
            ])
            .unwrap();
        let reply = model
            .chat("system", &[ChatMsg::user("inspect these")])
            .unwrap();
        let blocks: Vec<serde_json::Value> = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            blocks
                .iter()
                .map(|block| block["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["text", "text", "image", "audio"]
        );
        let attached = blocks[1]["text"].as_str().unwrap();
        let (path, rest) = attached
            .split_once("[Attached file: notes.txt — at ")
            .and_then(|(_, rest)| rest.split_once("]\n"))
            .unwrap_or_else(|| panic!("{attached}"));
        assert!(rest.starts_with("bounded context"), "{attached}");
        assert_eq!(std::fs::read(path).unwrap(), b"bounded context", "the seat reads it where the line says");
        assert!(attached.contains("spec.pdf (application/pdf"), "{attached}");
        assert!(attached.contains("open it with your tools"), "{attached}");
        assert!(attached.contains("photo.heic (image/heic"), "{attached}");
        assert_eq!(blocks[2]["hasData"], true);
        assert_eq!(blocks[3]["hasData"], true);
    }

    /// A native runtime's write to its own settings, or to a credential,
    /// never becomes a review item (none of its content is kept).
    #[test]
    fn native_review_refuses_a_runtimes_settings_and_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-settings"))
            .unwrap()
            .context
            .stream_id;
        let write = |path: &str| -> acp::RequestPermissionRequest {
            serde_json::from_value(serde_json::json!({
                "sessionId": "s",
                "options": [],
                "toolCall": {"toolCallId": "c", "rawInput": {"file_path": path, "content": "{}"}},
                "_meta": {"bakery.dev/review": {"version": NATIVE_REVIEW_VERSION, "toolName": "Write"}},
            }))
            .unwrap()
        };
        for path in [
            ".claude/settings.local.json",
            ".gemini/settings.json",
            ".env",
        ] {
            let refused = native_review_request(&state, &stream_id, &write(path), &[]);
            assert!(refused.is_err(), "{path} must be refused");
        }
        assert!(
            native_review_request(&state, &stream_id, &write("notes.md"), &[])
                .unwrap()
                .is_some()
        );
        // The operator saving it mid-turn never locks Claude out of it.
        let root = crate::state::jail_for(&state, &stream_id).unwrap().root().to_path_buf();
        crate::writers::turn_began(&stream_id);
        crate::writers::wrote(crate::writers::Writer::Operator, &root, "notes.md");
        assert!(native_review_request(&state, &stream_id, &write("notes.md"), &[]).unwrap().is_some());
        // The seat's own temp folder is no workspace change: the ordinary
        // gate takes it. A `..` never climbs out of it into Review's place.
        let tmp = [PathBuf::from("/seat/tmp")];
        assert!(native_review_request(&state, &stream_id, &write("/seat/tmp/claude/scratch.py"), &tmp).unwrap().is_none());
        // A link under the temp folder to a file outside it takes Review.
        let seat = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), seat.path().join("out")).unwrap();
        let tmp = [seat.path().to_path_buf()];
        let inside = |rel: &str| in_folders(&seat.path().join(rel).to_string_lossy(), &tmp);
        assert!(inside("scratch/new.py"));
        assert!(!inside("out/notes.md"), "through a link, outside");
        std::os::unix::fs::symlink(outside.path().join("not-yet.md"), seat.path().join("dangling.md")).unwrap();
        assert!(!inside("dangling.md"), "a link to a file not made yet is no file inside");
        assert!(native_review_request(&state, &stream_id, &write("/seat/tmp/../../etc/x"), &tmp).is_err());
    }

    /// Review sees a file as Claude Code's own Edit does, so whatever Claude
    /// Code can change, Review can show: every match for replace_all, CRLF
    /// and non-UTF-8 files, curly quotes, a new file, a file too large to
    /// keep whole, and one notebook cell.
    #[test]
    fn a_review_sees_the_file_as_claude_code_edits_it() {
        let input = |value: serde_json::Value| value.as_object().unwrap().clone();
        let edit = |old: &str, new: &str, all: bool| input(serde_json::json!({"old_string": old, "new_string": new, "replace_all": all}));
        let view = |tool: &str, input: &serde_json::Map<String, serde_json::Value>, bytes: &[u8]| review_view(tool, input, bytes, bytes.len() as u64, "f");

        let all = view("Edit", &edit("x", "y", true), b"x1 x2 x3\n").unwrap();
        assert_eq!((all.proposed.as_str(), all.whole), ("y1 y2 y3\n", true));
        let why = view("Edit", &edit("x", "y", false), b"x1 x2\n").err().unwrap();
        assert!(why.contains("occurs 2 times"), "{why}");

        let crlf = view("Edit", &edit("a\nb", "c", false), b"a\r\nb\r\n").unwrap();
        assert_eq!((crlf.old.as_str(), crlf.proposed.as_str()), ("a\nb\n", "c\n"));
        let latin = view("Edit", &edit("ok", "fine", false), b"caf\xe9 ok\n").unwrap();
        assert_eq!(latin.proposed, "caf\u{fffd} fine\n");
        let utf16: Vec<u8> = [0xff, 0xfe].into_iter().chain("hi\n".encode_utf16().flat_map(u16::to_le_bytes)).collect();
        assert_eq!(view("Edit", &edit("hi", "yo", false), &utf16).unwrap().proposed, "\u{feff}yo\n");
        let quotes = view("Edit", &edit("say \"hi\"", "say bye", false), "say \u{201c}hi\u{201d}\n".as_bytes()).unwrap();
        assert_eq!(quotes.proposed, "say bye\n");

        assert_eq!(view("Edit", &edit("", "new\n", false), b"").unwrap().proposed, "new\n");
        assert!(view("Edit", &edit("", "new\n", false), b"old\n").is_err());

        let big = vec![b'z'; crate::tools::MAX_EDIT_BYTES + 1];
        let snippet = view("Edit", &edit("zz", "yy", true), &big).unwrap();
        assert_eq!((snippet.old.as_str(), snippet.proposed.as_str(), snippet.whole), ("zz", "yy", false));
        let head = view("Write", &input(serde_json::json!({"content": "short\n"})), &big).unwrap();
        assert!(!head.whole && head.old.len() < REVIEW_HEAD_BYTES + 100 && head.old.contains("more bytes of f not shown"));

        let notebook = br#"{"cells": [{"id": "a", "cell_type": "code", "source": ["x = 1\n", "y"]}, {"id": "b", "cell_type": "markdown", "source": "md"}]}"#;
        let cell = |fields: serde_json::Value| view("NotebookEdit", &input(fields), notebook).map(|v| (v.old, v.proposed, v.whole));
        assert_eq!(cell(serde_json::json!({"cell_id": "a", "new_source": "x = 2"})).unwrap(), ("x = 1\ny".into(), "x = 2".into(), false));
        assert_eq!(cell(serde_json::json!({"cell_id": "cell-1", "new_source": "doc"})).unwrap().0, "md");
        assert_eq!(cell(serde_json::json!({"cell_id": "a", "new_source": "z", "edit_mode": "insert", "cell_type": "code"})).unwrap().0, "");
        assert_eq!(cell(serde_json::json!({"cell_id": "b", "new_source": "", "edit_mode": "delete"})).unwrap().1, "");

        // A big file is read only as far as its review shows; past the
        // ceiling, not at all.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("big.log");
        std::fs::File::create(&file).unwrap().set_len(crate::tools::MAX_EDIT_BYTES as u64 + 1).unwrap();
        let (head, size) = read_for_review(&file, "Write", "big.log").unwrap();
        assert_eq!(size, crate::tools::MAX_EDIT_BYTES as u64 + 1);
        assert!(head.len() <= REVIEW_HEAD_BYTES + 4);
        std::fs::File::options().write(true).open(&file).unwrap().set_len(REVIEW_CEILING_BYTES + 1).unwrap();
        assert!(read_for_review(&file, "Write", "big.log").unwrap_err().contains("past the"));
    }

    /// Accepted as proposed, Claude Code gets its own input back and applies
    /// its own edit; an operator's version takes its place; a refusal says why.
    #[test]
    fn a_reviewed_write_answers_with_claudes_input_the_operators_or_a_reason() {
        let options: Vec<acp::PermissionOption> = serde_json::from_value(serde_json::json!([
            {"optionId": "allow-once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject", "name": "No", "kind": "reject_once"},
        ]))
        .unwrap();
        let review = |tool: &str, input: serde_json::Value, whole: bool| NativeReviewRequest {
            tool_call_id: "c".into(),
            tool_name: tool.into(),
            file_path: "f".into(),
            old_content: "old\n".into(),
            proposed_content: "new\n".into(),
            whole,
            old_full: None,
            input,
        };
        let updated = |response: acp::RequestPermissionResponse| {
            serde_json::to_value(response).unwrap()["_meta"][NATIVE_REVIEW_META_KEY]["updatedInput"].clone()
        };
        let edit = serde_json::json!({"file_path": "f", "old_string": "o", "new_string": "n", "replace_all": true});
        assert_eq!(updated(native_review_response(&options, &review("Edit", edit.clone(), true), "new\n")), edit);
        let mine = updated(native_review_response(&options, &review("Edit", edit.clone(), true), "mine\n"));
        assert_eq!((&mine["old_string"], &mine["new_string"], &mine["replace_all"]), (&"old\n".into(), &"mine\n".into(), &false.into()));
        let snippet = updated(native_review_response(&options, &review("Edit", edit, false), "mine"));
        assert_eq!((&snippet["old_string"], &snippet["replace_all"]), (&"o".into(), &true.into()));
        let delete = serde_json::json!({"notebook_path": "n.ipynb", "cell_id": "b", "new_source": "", "edit_mode": "delete"});
        let kept = updated(native_review_response(&options, &review("NotebookEdit", delete, false), "kept"));
        assert_eq!((&kept["new_source"], &kept["edit_mode"]), (&"kept".into(), &"replace".into()));

        let refused = serde_json::to_value(review_refusal(&options, "c", "because", true)).unwrap();
        assert_eq!(refused["outcome"]["optionId"], "reject");
        assert_eq!(refused["_meta"][NATIVE_REVIEW_META_KEY]["reason"], "because");
        assert!(serde_json::to_value(review_refusal(&options, "c", "because", false)).unwrap().get("_meta").is_none());
    }

    /// Plan holds from the first turn when it was chosen before any runtime
    /// ran: choosing it starts none, and the runtime takes its own plan mode
    /// before its first prompt.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn plan_mode_denies_native_mutation_without_creating_an_approval_proposal() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-plan";
        let model = fake_model_in_workspace(
            &state,
            stream_id,
            tempfile::tempdir().unwrap().keep(),
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "edit".to_string()),
            ],
            true,
        );
        model.set_native_operating_mode(Some("plan")).unwrap();
        let runtime = || acp_runtime_snapshot(&state, stream_id, Seat::Orchestrator);
        assert!(runtime().is_none(), "choosing a mode starts no runtime");
        let reply = model
            .chat("system", &[ChatMsg::user("Explain only; make no edits")])
            .unwrap();
        assert!(reply.contains("reject-once"), "{reply}");
        assert_eq!(runtime().unwrap().current_mode.as_deref(), Some("plan"), "its own plan mode");
        assert!(
            pending_gate_events(&state, stream_id).is_empty(),
            "Plan must refuse mutation structurally, without surfacing an edit/approval proposal"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_review_accepts_human_edited_bytes_once_through_the_paused_tool_call() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened = open_stream(&state, &open_req(&repo, "native-review")).unwrap();
        let stream_id = opened.context.stream_id;
        let workspace = PathBuf::from(opened.context.worktree_path);
        let target = workspace.join("f.txt");
        let model = Arc::new(fake_model_in_workspace(
            &state,
            &stream_id,
            workspace,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_NATIVE_REVIEW".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "edit".to_string()),
            ],
            true,
        ));
        model.set_native_operating_mode(Some("manual")).unwrap();
        let turn_id = "native-review-turn";
        model.set_current_turn_id(turn_id);
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, &stream_id, &agent_id, turn_id);

        let worker = model.clone();
        let reply =
            std::thread::spawn(move || worker.chat("system", &[ChatMsg::user("make the edit")]));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let proposal = loop {
            let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
            if let Some(event) = events
                .iter()
                .find(|event| event.family == "agent.file.changed")
            {
                break event.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native review proposal never arrived"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let proposal_payload: serde_json::Value = serde_json::from_str(&proposal.payload).unwrap();
        assert_eq!(proposal_payload["old_content"], "line one\n");
        assert_eq!(proposal_payload["new_content"], "runtime proposal\n");
        assert_eq!(proposal_payload["native_review"]["tool_name"], "Edit");
        assert!(matches!(
            crate::state::agent_activity_snapshot(&state, &agent_id)
                .unwrap()
                .phase,
            crate::state::ActivityPhase::AwaitingApproval {
                kind: crate::state::ApprovalKind::FileEdit,
                ..
            }
        ));

        let decision = crate::routes::agent::decide_with_content(
            &state,
            &crate::routes::agent::AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: proposal.event_hash.clone(),
            },
            true,
            Some("human revision\n"),
        )
        .unwrap();
        let decision_payload: serde_json::Value = serde_json::from_str(&decision.payload).unwrap();
        assert_eq!(decision_payload["new_content"], "human revision\n");
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "line one\n",
            "Review records approval but never pre-writes a native operation"
        );

        let reply = reply.join().unwrap().unwrap();
        assert!(
            reply.contains("native review wrote: \"human revision\\n\""),
            "{reply}"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "human revision\n",
            "the runtime performed exactly the reviewed replacement once"
        );
        let proposal_after: serde_json::Value = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&stream_id)
            .unwrap()
            .into_iter()
            .find(|event| event.event_hash == proposal.event_hash)
            .and_then(|event| serde_json::from_str(&event.payload).ok())
            .unwrap();
        assert_eq!(
            proposal_after["new_content"], "runtime proposal\n",
            "the original proposal stays immutable"
        );
    }

    /// A native runtime's own compaction is a row on the thread, its summary
    /// with it, once however often the runtime says it ended.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_compaction_is_recorded_once_with_its_summary() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:native-compaction";
        let model = fake_model(&state, stream_id, vec![("BAKERY_FAKE_ACP_COMPACTS".to_string(), "1".to_string())]);
        model.chat("system", &[ChatMsg::user("keep going")]).unwrap();
        let rows: Vec<serde_json::Value> = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(stream_id, "agent.context.compacted")
            .unwrap()
            .iter()
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((&rows[0]["native"], &rows[0]["status"], &rows[0]["summary"]), (&true.into(), &"completed".into(), &"Built the parser.".into()));
        // Its size, which the update never carries, from the session's own
        // record, read just after the prompt's end.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let sizes = loop {
            let sizes = state.heart.lock().unwrap().events_of_family(stream_id, COMPACTION_SIZE).unwrap();
            if !sizes.is_empty() || std::time::Instant::now() > deadline {
                break sizes;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(sizes.len(), 1);
        let size: serde_json::Value = serde_json::from_str(&sizes[0].payload).unwrap();
        assert_eq!((&size["tokens"], &size["boundary"]), (&152_340.into(), &"boundary-1".into()), "{size}");
    }

    /// A native compaction's size comes from the session's own record (Claude
    /// Code's compact boundary), as the update never carries one: the entry
    /// nearest in time, each entry once. One whose entry is not written yet
    /// waits; as the next prompt starts it gets none, for good.
    #[test]
    fn a_native_compaction_gets_its_size_from_the_sessions_own_record() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (stream_id, seat, session) = ("wt:compaction-size", Seat::Orchestrator, "sess-1");
        let agent_id = crate::node::agent_id(stream_id, seat);
        let compacted = |id: &str, session: &str, agent: &str| {
            let p = serde_json::json!({"agent_id": agent, "turn_id": "t1", "native": true, "compaction_id": id, "session_id": session, "status": "completed"});
            crate::state::append(&state, stream_id, "agent:orchestrator", "agent.context.compacted", p.to_string(), vec![]).unwrap()
        };
        let record = crate::resume::session_copy(&state, &agent_id, session).join("HOME/.claude/projects/app").join(format!("{session}.jsonl"));
        std::fs::create_dir_all(record.parent().unwrap()).unwrap();
        let boundary = |uuid: &str, tokens: u64| {
            serde_json::json!({"type": "system", "subtype": "compact_boundary", "timestamp": chrono::Utc::now().to_rfc3339(), "uuid": uuid, "compactMetadata": {"trigger": "auto", "preTokens": tokens}})
                .to_string()
        };
        let mut lines = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"hi\"}}}}\n{}\n", boundary("b-1", 152_340));
        let sizes = || -> Vec<(Vec<String>, serde_json::Value)> {
            let events = state.heart.lock().unwrap().events_of_family(stream_id, COMPACTION_SIZE).unwrap();
            events.into_iter().map(|e| (e.causality, serde_json::from_str(&e.payload).unwrap())).collect()
        };

        let first = compacted("cmp-1", session, &agent_id);
        std::fs::write(&record, &lines).unwrap();
        let second = compacted("cmp-2", session, &agent_id);
        compacted("cmp-x", "another-session", &agent_id);
        compacted("cmp-y", session, "wt:compaction-size:baker");
        fill_compaction_sizes(&state, stream_id, seat, session, false);
        let found = sizes();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, [first.event_hash.clone()]);
        assert_eq!((&found[0].1["tokens"], &found[0].1["boundary"]), (&152_340.into(), &"b-1".into()));

        // The second's entry, written late, is found next time; the first
        // keeps its own.
        lines.push_str(&format!("{}\n", boundary("b-2", 412)));
        std::fs::write(&record, &lines).unwrap();
        fill_compaction_sizes(&state, stream_id, seat, session, false);
        let found = sizes();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!((&found[1].0, &found[1].1["tokens"]), (&vec![second.event_hash.clone()], &412.into()));

        // One its record never holds gets none once the next prompt starts.
        let third = compacted("cmp-3", session, &agent_id);
        fill_compaction_sizes(&state, stream_id, seat, session, false);
        assert_eq!(sizes().len(), 2, "it may still be written");
        fill_compaction_sizes(&state, stream_id, seat, session, true);
        let found = sizes();
        assert_eq!(found.len(), 3);
        assert_eq!((&found[2].0, &found[2].1["tokens"]), (&vec![third.event_hash], &serde_json::Value::Null));
        fill_compaction_sizes(&state, stream_id, seat, session, true);
        assert_eq!(sizes().len(), 3, "each once");
    }

    /// The operator's No reaches Claude as a reason, not the adapter's generic
    /// refusal, so it can change course.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_rejected_native_review_tells_the_runtime_why() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened = open_stream(&state, &open_req(&repo, "native-review-no")).unwrap();
        let stream_id = opened.context.stream_id;
        let model = Arc::new(fake_model_in_workspace(
            &state,
            &stream_id,
            PathBuf::from(opened.context.worktree_path),
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_NATIVE_REVIEW".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "edit".to_string()),
            ],
            true,
        ));
        model.set_native_operating_mode(Some("manual")).unwrap();
        let turn_id = "native-review-no-turn";
        model.set_current_turn_id(turn_id);
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, &stream_id, &agent_id, turn_id);
        let worker = model.clone();
        let reply = std::thread::spawn(move || worker.chat("system", &[ChatMsg::user("make the edit")]));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let proposal = loop {
            let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
            if let Some(event) = events.iter().find(|event| event.family == "agent.file.changed") {
                break event.clone();
            }
            assert!(std::time::Instant::now() < deadline, "native review proposal never arrived");
            std::thread::sleep(Duration::from_millis(10));
        };
        crate::routes::agent::decide_with_content(
            &state,
            &crate::routes::agent::AgentDecisionReq { stream_id: stream_id.clone(), event_hash: proposal.event_hash },
            false,
            None,
        )
        .unwrap();
        let reply = reply.join().unwrap().unwrap();
        assert!(reply.contains("native review refused: The operator rejected this change in Review."), "{reply}");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn stop_cancels_a_native_review_wait_before_a_late_accept_can_write() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened = open_stream(&state, &open_req(&repo, "native-review-stop")).unwrap();
        let stream_id = opened.context.stream_id;
        let workspace = PathBuf::from(opened.context.worktree_path);
        let target = workspace.join("f.txt");
        let model = Arc::new(fake_model_in_workspace(
            &state,
            &stream_id,
            workspace,
            vec![
                ("BAKERY_FAKE_ACP_CAPABILITIES".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_NATIVE_REVIEW".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "edit".to_string()),
            ],
            true,
        ));
        model.set_native_operating_mode(Some("manual")).unwrap();
        let turn_id = "native-review-stop-turn";
        model.set_current_turn_id(turn_id);
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, &stream_id, &agent_id, turn_id);

        let worker = model.clone();
        let reply =
            std::thread::spawn(move || worker.chat("system", &[ChatMsg::user("make the edit")]));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let proposal = loop {
            let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
            if let Some(event) = events
                .iter()
                .find(|event| event.family == "agent.file.changed")
            {
                break event.clone();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "native review proposal never arrived"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        let response = futures::executor::block_on(crate::routes::agent::agent_turn_cancel(
            axum::extract::State(state.clone()),
            axum::Json(crate::routes::agent::AgentTurnCancelReq {
                stream_id: stream_id.clone(),
                seat: Seat::Orchestrator,
            }),
        ))
        .unwrap();
        assert_eq!(response.0["cancelled"], true);
        assert_eq!(response.0["approval_cancelled"], true);
        let late_accept = crate::routes::agent::decide_with_content(
            &state,
            &crate::routes::agent::AgentDecisionReq {
                stream_id: stream_id.clone(),
                event_hash: proposal.event_hash.clone(),
            },
            true,
            Some("human revision\n"),
        )
        .unwrap_err();
        assert!(late_accept.contains("stopped"), "{late_accept}");

        let _reply = reply.join().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "line one\n",
            "the native runtime must receive cancellation, never a late reviewed write"
        );
        let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.family == "agent.approval.cancelled"
                        && event
                            .causality
                            .iter()
                            .any(|cause| cause == &proposal.event_hash)
                })
                .count(),
            1,
            "one native review wait gets one durable Stop settlement"
        );
    }

    /// 07b Proof #1: a workspace with no `mise.toml` at all still completes
    /// a full ACP round trip (initialize → prompt) inside the box.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn round_trip_succeeds_in_a_workspace_with_no_mise_toml() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let workspace = tempfile::tempdir().unwrap(); // deliberately no mise.toml
        let workspace_path = workspace.path().to_path_buf();
        assert!(!workspace_path.join("mise.toml").exists());
        let model = fake_model_in_thin_workspace(&state, "wt:thinacp", workspace_path);

        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(reply, "hello from fake agent");
    }

    /// 07b Proof #2: that same no-`mise.toml` boot performs no Rust toolchain
    /// copy — no `envs/<digest>/toolchain/rust` clone from the host sysroot.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn thin_room_boot_clones_no_rust_toolchain() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_path_buf();
        let model = fake_model_in_thin_workspace(&state, "wt:thinnoclone", workspace_path);

        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert!(reply.contains("hello from fake agent"));

        let envs_dir = state.base.join("cache").join("envs");
        let entries: Vec<_> = std::fs::read_dir(&envs_dir)
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        for entry in &entries {
            assert!(
                !entry.path().join("toolchain/rust/bin/rustc").is_file(),
                "thin boot must not clone a rust toolchain, found one at {:?}",
                entry.path()
            );
        }
    }

    /// 07b Proof #3: a workspace WITH `mise.toml` pinning a tool this backend
    /// cannot provision must still let the thin ACP spawn succeed — the
    /// pinned-tools route is independent and may fail on its own.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn thin_room_boots_even_when_mise_toml_pins_an_unsupported_tool() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(
            workspace.path().join("mise.toml"),
            "[tools]\nnode = \"22\"\n",
        )
        .unwrap();
        let workspace_path = workspace.path().to_path_buf();
        let model =
            fake_model_in_thin_workspace(&state, "wt:thinunsupported", workspace_path.clone());

        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(reply, "hello from fake agent");

        match bakery_exec::provision_project_tools(
            &workspace_path,
            &state.base.join("cache"),
            "wt:thinunsupported",
        ) {
            bakery_exec::ProjectToolsStatus::Unsupported(e) => assert!(e.contains("node")),
            _ => panic!("an unsupported pin must fail provision_project_tools, not thin boot"),
        }
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn round_trip_reflects_a_message_set_on_the_child_process_env() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(
            &state,
            "wt:pipeenv",
            vec![(
                "BAKERY_FAKE_ACP_MESSAGE".to_string(),
                "custom xyz".to_string(),
            )],
        );

        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(reply, "custom xyz");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_bakery_provider_secret_never_reaches_the_spawned_plug() {
        let _guard = bakery_agent::secrets::test_support::CREDENTIAL_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "ANTHROPIC_API_KEY",
            "test-only-value-must-not-leak-to-child",
        );
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(
            &state,
            "wt:pipesecret",
            vec![(
                "BAKERY_FAKE_ACP_CHECK_ENV_ABSENT".to_string(),
                "ANTHROPIC_API_KEY".to_string(),
            )],
        );

        let reply = model.chat("system", &[ChatMsg::user("hi")]);
        std::env::remove_var("ANTHROPIC_API_KEY");
        drop(_guard);
        let reply = reply.unwrap();
        assert_eq!(reply, "env var ANTHROPIC_API_KEY is absent");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn an_execute_kind_tool_call_is_denied_by_default_and_recorded_as_a_pending_gate() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipeexec";
        let model = fake_model(
            &state,
            stream_id,
            vec![(
                "BAKERY_FAKE_ACP_TOOL_KIND".to_string(),
                "execute".to_string(),
            )],
        );

        let reply = model
            .chat("system", &[ChatMsg::user("run something")])
            .unwrap();
        assert!(
            reply.contains("cancelled"),
            "an unapproved Execute must be denied: {reply}"
        );

        let pending = pending_gate_events(&state, stream_id);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["kind"], "execute");
        assert_eq!(pending[0]["plug"], "acp");
    }

    /// A gate parked on a human never holds the connection's one dispatch
    /// loop: the runtime keeps streaming meanwhile, and when it takes the
    /// request back (`$/cancel_request`) the card closes instead of
    /// lingering as an approval nothing is waiting for.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_withdrawn_permission_request_closes_its_gate_without_stalling_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipewithdraw";
        let model = Arc::new(fake_model(
            &state,
            stream_id,
            vec![
                (
                    "BAKERY_FAKE_ACP_TOOL_KIND".to_string(),
                    "execute".to_string(),
                ),
                (
                    "BAKERY_FAKE_ACP_WITHDRAW_PERMISSION".to_string(),
                    "1".to_string(),
                ),
            ],
        ));
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel =
            crate::state::start_agent_activity(&state, stream_id, &agent_id, "withdraw-turn");
        model.set_current_turn_id("withdraw-turn");

        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let worker = model.clone();
        std::thread::spawn(move || {
            let _ = reply_tx.send(worker.chat("system", &[ChatMsg::user("run something")]));
        });
        let reply = reply_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("a parked gate stalled the connection")
            .unwrap();
        assert!(reply.starts_with("still streaming while parked; "), "{reply}");
        assert!(reply.contains("-32800"), "answered as cancelled: {reply}");

        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let pending = events
            .iter()
            .find(|e| e.family == CHILD_TOOL_PENDING_FAMILY)
            .expect("the gate was raised");
        let closed = events
            .iter()
            .filter(|e| {
                e.family == "agent.approval.cancelled" && e.causality.contains(&pending.event_hash)
            })
            .count();
        assert_eq!(closed, 1);
        let late = decide_child_tool_gate(
            &state,
            stream_id,
            &pending.event_hash,
            ChildToolGateDecisionKind::AllowOnce,
        )
        .unwrap_err();
        assert!(late.contains("stopped"), "{late}");
    }

    /// Bakery remembers "always" itself; a runtime is only ever told "once",
    /// so it never writes approval rules into the project or its config.
    #[test]
    fn a_runtime_is_only_ever_told_once_even_for_always() {
        let options: Vec<acp::PermissionOption> = serde_json::from_value(serde_json::json!([
            {"optionId": "always", "name": "Always", "kind": "allow_always"},
            {"optionId": "once", "name": "Once", "kind": "allow_once"},
            {"optionId": "never", "name": "Never", "kind": "reject_always"},
            {"optionId": "no", "name": "No", "kind": "reject_once"},
        ]))
        .unwrap();
        let picked = |decision| match permission_outcome(&options, decision) {
            acp::RequestPermissionOutcome::Selected(selected) => selected.option_id.0.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(picked(ChildToolGateDecisionKind::AllowAlways), "once");
        assert_eq!(picked(ChildToolGateDecisionKind::AllowOnce), "once");
        assert_eq!(picked(ChildToolGateDecisionKind::Deny), "no");
    }

    /// A kind the operator never allows is refused on a session where
    /// everything else is approved automatically, and the refusal is
    /// recorded with its reason — never a silent drop.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_kind_you_never_allow_is_refused_even_under_automatic_approval() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state
            .node
            .lock()
            .unwrap()
            .denied_child_tools
            .push(ChildToolKind::Execute);
        let stream_id = "wt:pipepolicy";
        let workspace = tempfile::tempdir().unwrap();
        let model = fake_model_in_workspace(
            &state,
            stream_id,
            workspace.path().to_path_buf(),
            vec![(
                "BAKERY_FAKE_ACP_TOOL_KIND".to_string(),
                "execute".to_string(),
            )],
            true,
        );

        let reply = model
            .chat("system", &[ChatMsg::user("run something")])
            .unwrap();
        assert!(reply.contains("reject-once"), "{reply}");
        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let decided = events
            .iter()
            .find(|e| e.family == CHILD_TOOL_DECIDED_FAMILY)
            .expect("the refusal is recorded");
        assert_eq!(decided.actor, "node");
        assert!(
            decided.payload.contains("tool policy"),
            "{}",
            decided.payload
        );
    }

    /// "Never" reaches the kinds that are otherwise always safe, too.
    #[test]
    fn never_overrides_the_always_safe_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state
            .node
            .lock()
            .unwrap()
            .denied_child_tools
            .push(ChildToolKind::Fetch);
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:pipefetch",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Fetch)
            ),
            ChildToolDecision::Deny
        ));
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:pipefetch",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Read)
            ),
            ChildToolDecision::Allow
        ));
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_read_kind_tool_call_is_auto_allowed_with_no_pending_gate() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:piperead";
        let model = fake_model(
            &state,
            stream_id,
            vec![("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "read".to_string())],
        );

        let reply = model
            .chat("system", &[ChatMsg::user("read something")])
            .unwrap();
        assert!(
            reply.contains("selected"),
            "a Read call is auto-safe: {reply}"
        );
        assert!(pending_gate_events(&state, stream_id).is_empty());
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn an_unrecognized_tool_kind_fails_closed_to_the_gated_category_not_auto_safe() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipeweird";
        let model = fake_model(
            &state,
            stream_id,
            vec![(
                "BAKERY_FAKE_ACP_TOOL_KIND".to_string(),
                "some_future_kind".to_string(),
            )],
        );

        let reply = model
            .chat("system", &[ChatMsg::user("do something novel")])
            .unwrap();
        assert!(reply.contains("cancelled"), "{reply}");
        assert_eq!(pending_gate_events(&state, stream_id)[0]["kind"], "other");
    }

    /// 07d-A3B review #2: "ACP work is largely invisible to Bakery... native
    /// reads, commands, edits, locations, and outcomes are discarded." A
    /// `tool_call` announcement and its `tool_call_update` (carrying a
    /// diff) must both land as bounded Heart evidence, correlated by
    /// `tool_call_id`, with the assigned model id attached (not a generic
    /// `"acp"` literal — review #2's second half).
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_tool_calls_and_diffs_are_captured_as_bounded_heart_events() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipetoolcall";
        let workspace = tempfile::tempdir().unwrap();
        let model = AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![(
                    "BAKERY_FAKE_ACP_EMIT_TOOL_CALL".to_string(),
                    "1".to_string(),
                )],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp:codex".to_string(),
        );

        model
            .chat("system", &[ChatMsg::user("edit the file")])
            .unwrap();

        let events = acp_tool_call_events(&state, stream_id);
        assert_eq!(
            events.len(),
            2,
            "one tool_call + one tool_call_update: {events:?}"
        );

        let announce = &events[0];
        assert_eq!(announce["event"], "tool_call");
        assert_eq!(
            announce["model_id"], "acp:codex",
            "must carry the named plug's real id, not a generic 'acp' literal"
        );
        assert_eq!(announce["detail"]["toolCallId"], "tc-diff-1");
        assert_eq!(announce["detail"]["kind"], "edit");

        let update = &events[1];
        assert_eq!(update["event"], "tool_call_update");
        assert_eq!(update["detail"]["toolCallId"], "tc-diff-1");
        assert_eq!(update["detail"]["status"], "completed");
        let diff = &update["detail"]["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["oldText"], "before-content");
        assert_eq!(diff["newText"], "after-content");
    }

    /// Stop ends a turn waiting on a session that takes its time to resume,
    /// at once, not after the resume's whole wait.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn stop_ends_the_wait_for_a_slow_resume() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-slow-resume";
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        // One it resumed before: a session with a conversation to load.
        crate::resume::note_session(&state, stream_id, &agent_id, "acp", "fake-session", true);
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = Arc::new(fake_model(&state, stream_id, vec![env("BAKERY_FAKE_ACP_RESUMABLE", "1"), env("BAKERY_FAKE_ACP_RESUME_HANGS", "1")]));
        let _turn = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-slow");
        model.set_current_turn_id("turn-slow");
        let started = std::time::Instant::now();
        let waiting = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("system", &[ChatMsg::user("go on")]))
        };
        std::thread::sleep(Duration::from_secs(2));
        crate::state::request_cancel(&state, &agent_id);
        let ended = waiting.join().unwrap();
        assert!(ended.is_err(), "{ended:?}");
        assert!(started.elapsed() < Duration::from_secs(30), "stopped at once, not after the resume's wait");
    }

    /// A turn that ends with a command still open (Stop, a failed prompt)
    /// keeps what it had printed: the held output is recorded, not dropped.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_command_still_open_when_the_turn_ends_keeps_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-open-command";
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = fake_model(
            &state,
            stream_id,
            vec![env("BAKERY_FAKE_ACP_EMIT_TERMINAL", "1"), env("BAKERY_FAKE_ACP_TERMINAL_UNFINISHED", "1")],
        );
        model.set_current_turn_id("turn-open");
        model.chat("system", &[ChatMsg::user("run the tests")]).unwrap();
        let events = state.heart.lock().unwrap().events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY).unwrap();
        assert!(
            events.iter().any(|e| e.payload.contains("running 2 tests") && e.payload.contains("turn-open")),
            "the open command's output is kept: {:?}",
            events.iter().map(|e| &e.payload).collect::<Vec<_>>()
        );
    }

    /// A native command can be watched while it runs and lands in Heart in a
    /// few records: the live strip names the real command (not "Terminal")
    /// and its newest output line, the studio's Agent terminal shows
    /// `$ command`, the output and the exit code, and Heart holds the output
    /// back — the first chunks as one record once the command had run a
    /// second, the rest on the call's last update beside its exit code —
    /// never one record per chunk.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_command_is_watchable_while_it_runs_and_lands_once_in_heart() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipeterminal";
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = Arc::new(fake_model(
            &state,
            stream_id,
            vec![
                env("BAKERY_FAKE_ACP_EMIT_TERMINAL", "1"),
                env("BAKERY_FAKE_ACP_TERMINAL_PAUSE_MS", "1100"),
            ],
        ));
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-cmd");
        model.set_current_turn_id("turn-cmd");
        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("system", &[ChatMsg::user("run the tests")]))
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let phase = crate::state::agent_activity_snapshot(&state, &agent_id)
                .unwrap()
                .phase;
            if let crate::state::ActivityPhase::RunningTool {
                tool,
                call_id,
                preview,
                output,
                ..
            } = phase
            {
                if output.as_deref() == Some("running 2 tests") {
                    assert_eq!(tool, "Terminal");
                    assert_eq!(call_id, "tc-term-1");
                    assert_eq!(preview.as_deref(), Some("cargo test"));
                    break;
                }
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the running command never reached the live strip"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        running.join().unwrap().unwrap();
        // It ended (exit 101), so the strip lists it as a failed step.
        let done = crate::state::agent_activity_snapshot(&state, &agent_id).unwrap();
        assert_eq!(
            done.recent_completed_tools
                .iter()
                .map(|t| (t.tool.as_str(), t.ok))
                .collect::<Vec<_>>(),
            [("Terminal", false)]
        );

        let rest = "test a ... ok\ntest b ... \u{1b}[31mFAILED\u{1b}[0m\n";
        let terminal = crate::terminals::agent_terminal(&state, stream_id).unwrap();
        let (bytes, _, _) = terminal.replay_since(0);
        assert_eq!(
            crate::terminals::shown(&bytes),
            format!("$ cargo test\nrunning 2 tests\n{rest}[exit 101]\n").replace('\n', "\r\n")
        );
        // Its block is marked with the runtime's own call id, the one its
        // row in the thread carries.
        assert!(String::from_utf8_lossy(&bytes).starts_with("\u{1b}]7766;tc-term-1\u{7}$ cargo test"));

        let records: Vec<serde_json::Value> = acp_tool_call_events(&state, stream_id)
            .into_iter()
            .filter(|e| e["detail"]["toolCallId"] == "tc-term-1")
            .collect();
        assert_eq!(
            records
                .iter()
                .map(|e| e["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["tool_call", "tool_call_update", "tool_call_update", "tool_call_update"],
            "announce, command, held output, end — not one per chunk: {records:?}"
        );
        assert_eq!(
            records[0]["detail"]["_meta"]["terminal_info"]["terminal_id"], "tc-term-1",
            "the runtime reports a terminal only to a client that advertised it"
        );
        assert_eq!(
            records[2]["detail"]["_meta"]["terminal_output"]["data"],
            "running 2 tests\n"
        );
        let end = &records[3]["detail"];
        assert_eq!(end["status"], "failed");
        assert_eq!(end["_meta"]["terminal_exit"]["exit_code"], 101);
        assert_eq!(end["_meta"]["terminal_output"]["data"], rest);
    }

    /// A seat's gate client with no process behind it: what its callbacks do
    /// with the updates a runtime sends.
    fn gate_client(state: &AppState, stream_id: &str, seat: Seat) -> GateClient {
        GateClient {
            state: state.clone(),
            stream_id: stream_id.to_string(),
            seat,
            generation: 1,
            model_id: "acp".to_string(),
            text: Arc::new(Mutex::new(String::new())),
            text_flushed: Arc::new(AtomicUsize::new(0)),
            current_turn_id: Arc::new(Mutex::new(String::new())),
            current_delta_tx: Arc::new(Mutex::new(None)),
            shared: Arc::new(Mutex::new(AcpSessionShared::new("acp".to_string(), 1))),
            trusted: true,
            commands: Arc::new(Mutex::new(BTreeMap::new())),
            subagent_calls: Arc::new(Mutex::new(BTreeMap::new())),
            subagent_text: Arc::new(Mutex::new(BTreeMap::new())),
            tool_titles: Arc::new(Mutex::new(BTreeMap::new())),
            thinking: Arc::new(Mutex::new(Thinking::default())),
            compactions: Mutex::new(BTreeMap::new()),
        }
    }

    /// The native calls Stop cuts short say how far each got: one caught at
    /// its card never ran (recorded with what the request says of it, so it
    /// reads whole unannounced); one that fails as Stop tears it down, and
    /// one still open as the prompt ends, had started. One that finished,
    /// and every call of a turn nobody stopped, are left as they are.
    #[test]
    fn native_calls_stop_cut_short_say_how_far_each_got() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-stopped-calls";
        let seat = Seat::Orchestrator;
        let client = gate_client(&state, stream_id, seat);
        // Its session is the seat's current one: its updates count.
        state.acp_sessions.lock().unwrap().insert((stream_id.to_string(), seat), AcpSlot::Closing { generation: 1 });
        let agent_id = crate::node::agent_id(stream_id, seat);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-s");
        *client.current_turn_id.lock().unwrap() = "turn-s".to_string();
        let notify = |update: serde_json::Value| {
            let note = serde_json::json!({"sessionId": "s", "update": update});
            client.session_notification(serde_json::from_value(note).unwrap()).unwrap();
        };
        let call = |id: &str, title: &str| serde_json::json!({"sessionUpdate": "tool_call", "toolCallId": id, "title": title, "kind": "execute", "status": "in_progress"});
        let marks = || -> Vec<(String, String)> {
            let events = state.heart.lock().unwrap().events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY).unwrap();
            events
                .iter()
                .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap())
                .filter(|p| p["event"] == STOPPED_BEFORE || p["event"] == STOPPED_AFTER)
                .map(|p| (p["event"].as_str().unwrap().to_string(), p["detail"]["toolCallId"].as_str().unwrap().to_string()))
                .collect()
        };

        // Before any Stop, a failure is a failure.
        notify(call("failed-alone", "make"));
        notify(serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "failed-alone", "status": "failed"}));
        for id in ["running", "torn", "finished"] {
            notify(call(id, id));
        }
        notify(serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "finished", "status": "completed"}));
        let recorded = state.heart.lock().unwrap().events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY).unwrap().len();
        assert_eq!(recorded, 6, "the updates were recorded");
        assert!(marks().is_empty());

        assert!(crate::state::request_cancel(&state, &agent_id));
        let held: acp::ToolCallUpdate =
            serde_json::from_value(serde_json::json!({"toolCallId": "held", "title": "rm -rf build", "kind": "execute"})).unwrap();
        client.stopped_before("turn-s", &held);
        notify(serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "torn", "status": "failed"}));
        notify(serde_json::json!({"sessionUpdate": "tool_call_update", "toolCallId": "held", "status": "failed"}));
        record_stopped_calls(&state, stream_id, seat, "acp", "turn-s", &client.tool_titles);
        let mut got = marks();
        got.sort();
        assert_eq!(
            got,
            [("stopped_after".to_string(), "running".to_string()), ("stopped_after".to_string(), "torn".to_string()), ("stopped_before".to_string(), "held".to_string())]
        );
        let events = state.heart.lock().unwrap().events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY).unwrap();
        let before: serde_json::Value = events
            .iter()
            .map(|e| serde_json::from_str::<serde_json::Value>(&e.payload).unwrap())
            .find(|p| p["event"] == STOPPED_BEFORE)
            .unwrap();
        assert_eq!((&before["detail"]["title"], &before["turn_id"]), (&"rm -rf build".into(), &"turn-s".into()));
        assert!(client.tool_titles.lock().unwrap().is_empty(), "each said once");
        assert!(stop_asked(&state, stream_id, seat, "turn-s") && !stop_asked(&state, stream_id, seat, "turn-other"));
    }

    /// Stop on a native call parked at its permission card: it never ran,
    /// and its record says so, with what the request says of it.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_call_stopped_at_its_card_never_ran() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-stop-at-card";
        let model = Arc::new(fake_model(&state, stream_id, vec![("BAKERY_FAKE_ACP_TOOL_KIND".to_string(), "execute".to_string())]));
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "card-turn");
        model.set_current_turn_id("card-turn");
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let worker = model.clone();
        std::thread::spawn(move || {
            let _ = reply_tx.send(worker.chat("system", &[ChatMsg::user("run something")]));
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while state.heart.lock().unwrap().events_of_family(stream_id, CHILD_TOOL_PENDING_FAMILY).unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline, "the card was never raised");
            std::thread::sleep(Duration::from_millis(20));
        }
        crate::routes::agent::stop_turn(&state, stream_id, Seat::Orchestrator).unwrap();
        let _ = reply_rx.recv_timeout(Duration::from_secs(10)).expect("the prompt never ended");
        let records: Vec<serde_json::Value> = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY)
            .unwrap()
            .iter()
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        let stopped: Vec<&serde_json::Value> = records.iter().filter(|p| p["event"] == STOPPED_BEFORE || p["event"] == STOPPED_AFTER).collect();
        assert_eq!(stopped.len(), 1, "{records:?}");
        assert_eq!((&stopped[0]["event"], &stopped[0]["detail"]["toolCallId"]), (&STOPPED_BEFORE.into(), &"tc-1".into()));
        assert_eq!(stopped[0]["detail"]["title"], "fake execute tool call");
    }

    /// Stop on a native prompt while one of its calls runs: the call had
    /// started, and what it reported so far stays its result.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_call_running_as_stop_lands_had_started() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-stop-while-running";
        let env = vec![
            ("BAKERY_FAKE_ACP_HOLD_PROMPT".to_string(), "1".to_string()),
            ("BAKERY_FAKE_ACP_HOLD_TOOL".to_string(), "1".to_string()),
        ];
        let model = Arc::new(fake_model(&state, stream_id, env));
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "hold-turn");
        model.set_current_turn_id("hold-turn");
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let worker = model.clone();
        std::thread::spawn(move || {
            let reply = worker.chat_turn_streaming("system", &[ChatMsg::user("build it")], &[], &mut |_| {}, &cancel);
            let _ = reply_tx.send(reply);
        });
        let records = || -> Vec<serde_json::Value> {
            let events = state.heart.lock().unwrap().events_of_family(stream_id, ACP_TOOL_EVENT_FAMILY).unwrap();
            events.iter().map(|e| serde_json::from_str(&e.payload).unwrap()).collect()
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !records().iter().any(|p| p["detail"]["rawOutput"] == "compiled 3 of 9") {
            assert!(std::time::Instant::now() < deadline, "the call never ran");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(crate::state::request_cancel(&state, &agent_id));
        let _ = reply_rx.recv_timeout(Duration::from_secs(15)).expect("the prompt never ended");
        let stopped: Vec<serde_json::Value> = records().into_iter().filter(|p| p["event"] == STOPPED_BEFORE || p["event"] == STOPPED_AFTER).collect();
        assert_eq!(stopped.len(), 1, "{stopped:?}");
        assert_eq!((&stopped[0]["event"], &stopped[0]["detail"]["toolCallId"]), (&STOPPED_AFTER.into(), &"tc-held".into()));
    }

    /// The commands of two seats of one stream, running side by side, share
    /// the stream's one Agent terminal: each one's output lands under its
    /// own `$ command` header, not under whichever command printed last.
    #[test]
    fn two_seats_commands_keep_their_output_under_their_own_headers() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-two-seats";
        let (a, b) = (
            gate_client(&state, stream_id, Seat::Orchestrator),
            gate_client(&state, stream_id, Seat::Worker),
        );
        let announce = |id: &str, command: &str| -> acp::ToolCall {
            serde_json::from_value(serde_json::json!({
                "toolCallId": id,
                "title": "Terminal",
                "kind": "execute",
                "status": "pending",
                "rawInput": {"command": command},
                "_meta": {"terminal_info": {"terminal_id": id}},
            }))
            .unwrap()
        };
        let update = |id: &str, meta: serde_json::Value| -> acp::ToolCallUpdate {
            serde_json::from_value(serde_json::json!({"toolCallId": id, "_meta": meta})).unwrap()
        };
        let chunk = |id: &str, data: &str| {
            update(id, serde_json::json!({"terminal_output_delta": {"terminal_id": id, "data": data}}))
        };
        let end = |id: &str| {
            let mut ended = update(id, serde_json::json!({"terminal_exit": {"terminal_id": id, "exit_code": 0}}));
            ended.fields.status = Some(acp::ToolCallStatus::Completed);
            ended
        };

        a.start_command("turn-a", &announce("call-a", "cargo test"));
        b.start_command("turn-b", &announce("call-b", "ls"));
        a.follow_command("turn-a", &mut chunk("call-a", "running 2 tests\n"));
        b.follow_command("turn-b", &mut chunk("call-b", "src\n"));
        a.follow_command("turn-a", &mut end("call-a"));
        b.follow_command("turn-b", &mut end("call-b"));

        let terminal = crate::terminals::agent_terminal(&state, stream_id).unwrap();
        let (bytes, _, _) = terminal.replay_since(0);
        assert_eq!(
            crate::terminals::shown(&bytes),
            [
                "$ cargo test\n",
                "$ ls\n",
                "$ cargo test\nrunning 2 tests\n",
                "$ ls\nsrc\n",
                "$ cargo test\n[exit 0]\n",
                "$ ls\n[exit 0]\n",
            ]
            .concat()
            .replace('\n', "\r\n")
        );
    }

    /// A native turn's cost is how far the runtime's cumulative session cost
    /// moved across its prompt — never the running total, and never a guess:
    /// no new reading, another currency, or a total that went down (`/clear`)
    /// leaves it absent.
    #[test]
    fn a_native_turns_cost_is_the_move_in_the_session_total() {
        let usd = |amount| acp::Cost::new(amount, "USD");
        let cost = |before: Option<&acp::Cost>, after: Option<&acp::Cost>| {
            native_turn_usage(None, before, after, None)
                .and_then(|(_, detail)| detail.cost)
                .map(|cost| (cost.amount, cost.currency))
        };
        assert_eq!(
            cost(None, Some(&usd(0.25))),
            Some((0.25, "USD".to_string())),
            "a fresh session had spent nothing before"
        );
        assert_eq!(
            cost(Some(&usd(0.25)), Some(&usd(0.75))),
            Some((0.5, "USD".to_string()))
        );
        assert_eq!(cost(Some(&usd(0.25)), Some(&usd(0.25))), None, "no new reading");
        assert_eq!(cost(Some(&usd(0.75)), Some(&usd(0.25))), None, "a reset total");
        assert_eq!(
            cost(Some(&acp::Cost::new(1.0, "EUR")), Some(&usd(2.0))),
            None,
            "another currency"
        );
        assert!(native_turn_usage(None, None, None, None).is_none());
    }

    /// A runtime that reports every count as zero (a `/compact` turn) has
    /// reported none: no tokens are recorded, and its cost still is.
    #[test]
    fn a_native_turn_with_every_count_zero_reports_no_tokens() {
        let zero = acp::Usage::new(0, 0, 0);
        assert!(native_turn_usage(Some(&zero), None, None, None).is_none());
        let usd = acp::Cost::new(0.25, "USD");
        let (tokens, detail) = native_turn_usage(Some(&zero), None, Some(&usd), None).unwrap();
        assert_eq!((tokens.tokens_in, tokens.tokens_out), (None, None));
        assert!(detail.cost.is_some());
        let read = acp::Usage::new(9_000, 12, 30);
        let (tokens, _) = native_turn_usage(Some(&read), None, None, None).unwrap();
        assert_eq!((tokens.tokens_in, tokens.tokens_out), (Some(12), Some(30)));
    }

    /// A runtime that can resume its session gets the same conversation
    /// back after its process ends, with no catch-up: even with its own
    /// transcript gone, since the Bakery kept a copy.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_session_resumes_from_the_bakerys_own_copy() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-resume"))
            .unwrap()
            .context
            .stream_id;
        let workspace = tempfile::tempdir().unwrap();
        let seat_a_runtime = || {
            let env = vec![("BAKERY_FAKE_ACP_RESUMABLE".to_string(), "1".to_string())];
            let model: Arc<dyn Model> =
                Arc::new(fake_model_in_workspace(&state, &stream_id, workspace.path().to_path_buf(), env, false));
            crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model);
        };
        seat_a_runtime();
        let turn = |text: &str| {
            let done = crate::routes::agent::agent_turn_inner(
                &state,
                &crate::routes::agent::AgentTurnReq {
                    stream_id: stream_id.clone(),
                    file_path: None,
                    instruction: text.into(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
            )
            .unwrap();
            serde_json::from_str::<serde_json::Value>(&done.payload).unwrap()["summary"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(turn("one"), "remembered 1 prompts");

        // Its process ends, and the runtime's own transcript is lost with it.
        crate::state::restart_acp_runtimes(&state, &stream_id);
        seat_a_runtime();
        let home = bakery_exec::seat_acp_home(&crate::state::cache_root(&state), &stream_id, "orchestrator");
        let transcript = home.join(".fake/sessions/fake-session.jsonl");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::fs::remove_file(&transcript).is_err() {
            assert!(std::time::Instant::now() < deadline, "no transcript at {}", transcript.display());
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert_eq!(turn("two"), "remembered 2 prompts", "the same session, put back by the Bakery");
        let told = std::fs::read_to_string(transcript.with_extension("last-prompt")).unwrap();
        assert!(!told.contains("this CLI process just (re)started"), "no catch-up: {told}");
        let opened: Vec<serde_json::Value> = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&stream_id, crate::resume::SESSION_OPENED)
            .unwrap()
            .iter()
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        let resumed: Vec<bool> = opened.iter().map(|p| p["resumed"].as_bool().unwrap()).collect();
        assert_eq!(resumed, vec![false, true]);

        // One that doesn't resume is said on the thread, and its turn stops
        // there until the operator chooses; never a new session on its own.
        crate::state::restart_acp_runtimes(&state, &stream_id);
        let env = vec![
            ("BAKERY_FAKE_ACP_RESUMABLE".to_string(), "1".to_string()),
            ("BAKERY_FAKE_ACP_RESUME_FAILS".to_string(), "1".to_string()),
        ];
        let model: Arc<dyn Model> = Arc::new(fake_model_in_workspace(&state, &stream_id, workspace.path().to_path_buf(), env, false));
        crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model);
        let _ = crate::routes::agent::agent_turn_inner(
            &state,
            &crate::routes::agent::AgentTurnReq {
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: "three".into(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
        );
        let family = |name: &str| -> Vec<serde_json::Value> {
            let heart = state.heart.lock().unwrap();
            heart.events_of_family(&stream_id, name).unwrap().iter().map(|e| serde_json::from_str(&e.payload).unwrap()).collect()
        };
        let lost = family(crate::resume::NOT_RESUMED);
        let lost = lost.last().expect("the lost session is said");
        assert_eq!(lost["session_id"], opened[1]["session_id"]);
        assert_eq!(lost["awaiting"], true);
        assert!(lost["reason"].as_str().unwrap().contains("not found"), "{lost}");
        let failed = family("agent.turn.failed");
        assert!(failed.last().unwrap()["reason"].as_str().unwrap().contains(crate::resume::NOT_RESUMED_ERROR), "{failed:?}");
        assert_eq!(family(crate::resume::SESSION_OPENED).len(), 2, "no new session without the operator");

        // The operator starts fresh: recorded, and the turn carries on in a
        // new session briefed from the thread.
        let choose = |choice| crate::routes::agent::SessionChoiceReq { stream_id: stream_id.clone(), seat: None, choice };
        crate::routes::agent::agent_session_choose_inner(&state, &choose(crate::routes::agent::SessionChoice::Fresh)).unwrap();
        assert_eq!(family(crate::resume::SESSION_CHOSEN).last().unwrap()["choice"], "fresh");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while family(crate::resume::SESSION_OPENED).len() < 3 || family("agent.turn.done").len() < 3 {
            assert!(std::time::Instant::now() < deadline, "the fresh session never carried the turn on");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(family(crate::resume::SESSION_OPENED).last().unwrap()["resumed"], false);
        assert!(
            crate::routes::agent::agent_session_choose_inner(&state, &choose(crate::routes::agent::SessionChoice::Retry)).is_err(),
            "nothing waits for a choice any more"
        );
    }

    /// The operator's choice wins over a resume already under way: the pane
    /// asked for the runtime again while the question waited, and that
    /// attempt failed only after the operator chose to start fresh. It used
    /// to ask about the same session a second time, on a card whose buttons
    /// the node then refused. Now it asks nothing more, and the one choice
    /// opens one fresh session, which the stopped turn carries on in.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_resume_under_way_when_the_operator_chooses_asks_nothing_more() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "resume-race")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        // One it resumed before: a session with a conversation to lose.
        crate::resume::note_session(&state, &stream_id, &agent_id, "acp", "fake-session", true);
        let workspace = tempfile::tempdir().unwrap();
        let seat_runtime = |waits: bool| -> Arc<dyn Model> {
            let mut env = vec![
                ("BAKERY_FAKE_ACP_RESUMABLE".to_string(), "1".to_string()),
                ("BAKERY_FAKE_ACP_RESUME_FAILS".to_string(), "1".to_string()),
            ];
            if waits {
                env.push(("BAKERY_FAKE_ACP_RESUME_WAITS".to_string(), "1".to_string()));
            }
            let model: Arc<dyn Model> =
                Arc::new(fake_model_in_workspace(&state, &stream_id, workspace.path().to_path_buf(), env, false));
            crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model.clone());
            model
        };
        let family = |name: &str| -> Vec<serde_json::Value> {
            let heart = state.heart.lock().unwrap();
            heart.events_of_family(&stream_id, name).unwrap().iter().map(|e| serde_json::from_str(&e.payload).unwrap()).collect()
        };

        // A turn stops on the session that didn't resume, and asks.
        seat_runtime(false);
        let stopped = crate::routes::agent::agent_turn_inner(
            &state,
            &crate::routes::agent::AgentTurnReq {
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: "one".into(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
        );
        assert!(stopped.is_err());
        assert_eq!(family(crate::resume::NOT_RESUMED).len(), 1);

        // The pane asks for the runtime: its resume is under way when the
        // operator starts fresh, and fails only after that.
        let pane = seat_runtime(true);
        let asked = std::thread::spawn(move || pane.ensure_runtime_ready());
        let home = bakery_exec::seat_acp_home(&crate::state::cache_root(&state), &stream_id, "orchestrator");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !home.join(".fake/resume-waiting").exists() {
            assert!(std::time::Instant::now() < deadline, "the second resume never started");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let fresh = crate::routes::agent::SessionChoiceReq {
            stream_id: stream_id.clone(),
            seat: None,
            choice: crate::routes::agent::SessionChoice::Fresh,
        };
        crate::routes::agent::agent_session_choose_inner(&state, &fresh).unwrap();
        std::fs::write(home.join(".fake/resume-go"), "").unwrap();

        asked.join().unwrap().expect("the attempt under way took the operator's choice");
        while family("agent.turn.done").is_empty() {
            assert!(std::time::Instant::now() < deadline, "the stopped turn never carried on");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(family(crate::resume::NOT_RESUMED).len(), 1, "one failure, one question");
        let opened = family(crate::resume::SESSION_OPENED);
        assert_eq!(opened.len(), 2, "one fresh session for one choice: {opened:?}");
        assert_eq!(opened[1]["resumed"], false);
    }

    /// A turn on a seat whose runtime can resume, and what its session
    /// records say: the reply, whether each session opened resumed, and how
    /// many questions were asked.
    fn native_turn(state: &AppState, stream_id: &str, mut env: Vec<(String, String)>) -> (String, Vec<bool>, usize) {
        env.push(("BAKERY_FAKE_ACP_RESUMABLE".to_string(), "1".to_string()));
        let model: Arc<dyn Model> = Arc::new(fake_model(state, stream_id, env));
        crate::state::seed_seat_model_for_test(state, stream_id, Seat::Orchestrator, model);
        let req = crate::routes::agent::AgentTurnReq {
            stream_id: stream_id.to_string(),
            file_path: None,
            instruction: "carry on".into(),
            seat: None,
            model_id: None,
            effort: None,
            actor: None,
        };
        let done = crate::routes::agent::agent_turn_inner(state, &req).unwrap();
        let heart = state.heart.lock().unwrap();
        let payloads = |family: &str| -> Vec<serde_json::Value> {
            heart.events_of_family(stream_id, family).unwrap().iter().map(|e| serde_json::from_str(&e.payload).unwrap()).collect()
        };
        let reply = serde_json::from_str::<serde_json::Value>(&done.payload).unwrap()["summary"].as_str().unwrap().to_string();
        let resumed = payloads(crate::resume::SESSION_OPENED).iter().map(|p| p["resumed"] == true).collect();
        (reply, resumed, payloads(crate::resume::NOT_RESUMED).len())
    }

    /// A session nothing was ever asked in is not resumed: its runtime kept
    /// nothing of it (Claude saves a session from its first message), so the
    /// resume could only fail and ask. The pane opened this one, then the
    /// runtime went; the next turn opens a fresh one, with no question.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_session_never_asked_anything_opens_fresh_without_a_question() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "never-asked")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        crate::resume::note_session(&state, &stream_id, &agent_id, "acp", "fake-session", false);
        let fails = vec![("BAKERY_FAKE_ACP_RESUME_FAILS".to_string(), "1".to_string())];
        let (reply, resumed, asked) = native_turn(&state, &stream_id, fails);
        assert_eq!(reply, "remembered 1 prompts", "a fresh session");
        assert_eq!((resumed, asked), (vec![false, false], 0), "nothing to ask");
    }

    /// A session moved in from Claude Code holds its conversation though no
    /// turn of the Bakery's ran in it yet: its first turn here resumes it.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_session_moved_in_resumes_before_any_turn_here() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "moved-in")).unwrap().context.stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        // As the move-in leaves it: the conversation where its runtime reads
        // sessions, the move-in noted, and the session to resume.
        let home = bakery_exec::seat_acp_home(&crate::state::cache_root(&state), &stream_id, "orchestrator");
        std::fs::create_dir_all(home.join(".fake/sessions")).unwrap();
        std::fs::write(home.join(".fake/sessions/fake-session.jsonl"), "prompt\nprompt\n").unwrap();
        let moved = serde_json::json!({"session_id": "fake-session", "title": "Moved in"});
        crate::state::append(&state, &stream_id, "human", crate::move_in::IMPORTED, moved.to_string(), vec![]).unwrap();
        crate::resume::note_session(&state, &stream_id, &agent_id, "acp", "fake-session", false);
        let (reply, resumed, asked) = native_turn(&state, &stream_id, vec![]);
        assert_eq!(reply, "remembered 3 prompts", "its own conversation, carried on");
        assert_eq!((resumed, asked), (vec![false, true], 0));
    }

    /// A native turn cut off by an overloaded provider is waited out and
    /// carried on in the same turn: the failure took the runtime's process,
    /// so the next one is told the turn from its record.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_turn_cut_off_by_an_overloaded_provider_carries_on() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-outage"))
            .unwrap()
            .context
            .stream_id;
        let workspace = tempfile::tempdir().unwrap();
        // In the seat's home, which its runtime may write.
        let home = bakery_exec::seat_acp_home(&crate::state::cache_root(&state), &stream_id, "orchestrator");
        let marker = home.join("overloaded-once");
        let at = format!("{}/overloaded-once", bakery_exec::ROOM_HOME_ENV_VALUE);
        let env = vec![("BAKERY_FAKE_ACP_OVERLOADED_ONCE".to_string(), at)];
        let model: Arc<dyn Model> =
            Arc::new(fake_model_in_workspace(&state, &stream_id, workspace.path().to_path_buf(), env, false));
        crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model);
        let done = crate::routes::agent::agent_turn_inner(
            &state,
            &crate::routes::agent::AgentTurnReq {
                stream_id: stream_id.clone(),
                file_path: None,
                instruction: "build the parser".into(),
                seat: None,
                model_id: None,
                effort: None,
                actor: None,
            },
        )
        .unwrap();
        assert_eq!(done.family, "agent.turn.done");
        let retries = state
            .heart
            .lock()
            .unwrap()
            .events_of_family(&stream_id, crate::resume::RETRYING)
            .unwrap();
        assert_eq!(retries.len(), 1);
        assert!(retries[0].payload.contains("overloaded"), "{}", retries[0].payload);
        let told = std::fs::read_to_string(format!("{}.prompt", marker.display())).unwrap();
        assert!(
            told.contains("RESUME after a cut-off") && told.contains("build the parser"),
            "{told}"
        );
    }

    /// A native turn records the same `agent.turn.usage` an HTTP turn does,
    /// from the runtime's own figures: the prompt's tokens, cache reads and
    /// writes, the runtime's context reading, and the turn's own cost — the
    /// second turn's $0.25, not the $0.50 running total. Thought tokens were
    /// never reported, so they stay absent.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_turn_records_its_usage_cost_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-usage"))
            .unwrap()
            .context
            .stream_id;
        let model: Arc<dyn Model> = Arc::new(fake_model(
            &state,
            &stream_id,
            vec![("BAKERY_FAKE_ACP_USAGE".to_string(), "1".to_string())],
        ));
        crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, model);
        let turn = || {
            crate::routes::agent::agent_turn_inner(
                &state,
                &crate::routes::agent::AgentTurnReq {
                    stream_id: stream_id.clone(),
                    file_path: None,
                    instruction: "hi".into(),
                    seat: None,
                    model_id: None,
                    effort: None,
                    actor: None,
                },
            )
            .unwrap()
        };
        assert_eq!(turn().family, "agent.turn.done");
        assert_eq!(turn().family, "agent.turn.done");

        let usage: Vec<serde_json::Value> = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&stream_id)
            .unwrap()
            .iter()
            .filter(|e| e.family == "agent.turn.usage")
            .map(|e| serde_json::from_str(&e.payload).unwrap())
            .collect();
        assert_eq!(usage.len(), 2, "{usage:?}");
        for p in &usage {
            assert_eq!(p["provider"], "acp");
            assert_eq!(p["tokens_in"], 12);
            assert_eq!(p["tokens_out"], 30);
            assert_eq!(p["cache_read_tokens"], 9000);
            assert_eq!(p["cache_write_tokens"], 400);
            assert!(p["thought_tokens"].is_null(), "never reported: {p}");
            assert_eq!(p["context_used"], 9442);
            assert_eq!(p["context_window"], 200000);
            assert_eq!(
                p["cost"],
                serde_json::json!({"amount": 0.25, "currency": "USD"})
            );
            assert!(p["duration_ms"].is_u64(), "{p}");
        }
    }

    /// A native turn's words land in Heart where they were written — the
    /// text before a tool call ahead of it, the rest after — not as one block
    /// behind every call; the full reply keeps a paragraph break at the split.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_text_lands_in_order_around_the_tool_calls_it_introduces() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipetextorder";
        let workspace = tempfile::tempdir().unwrap();
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![
                    env("BAKERY_FAKE_ACP_EMIT_TOOL_CALL", "1"),
                    env("BAKERY_FAKE_ACP_TEXT_BEFORE_TOOL", "Editing it now."),
                    env("BAKERY_FAKE_ACP_MESSAGE", "Done."),
                ],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp:codex".to_string(),
        );
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-1");
        model.set_current_turn_id("turn-1");

        let reply = model
            .chat("system", &[ChatMsg::user("edit the file")])
            .unwrap();
        assert_eq!(reply, "Editing it now.\n\nDone.");
        // The live text is painted in order with the tool call: only the
        // words after it are still live, never the ones already recorded.
        match crate::state::agent_activity_snapshot(&state, &agent_id).unwrap().phase {
            crate::state::ActivityPhase::Streaming { text } => assert_eq!(text, "Done."),
            other => panic!("expected the live text after the tool call, got {other:?}"),
        }

        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let order: Vec<String> = events
            .iter()
            .filter(|e| e.family == ACP_TEXT_EVENT_FAMILY || e.family == ACP_TOOL_EVENT_FAMILY)
            .map(|e| {
                let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
                if e.family == ACP_TEXT_EVENT_FAMILY {
                    assert_eq!(p["turn_id"], "turn-1");
                    format!("text:{}", p["text"].as_str().unwrap())
                } else {
                    format!("tool:{}", p["event"].as_str().unwrap())
                }
            })
            .collect();
        assert_eq!(
            order,
            [
                "text:Editing it now.",
                "tool:tool_call",
                "tool:tool_call_update",
                "text:Done."
            ]
        );
    }

    /// A subagent works the way Claude Code shows it: its words and steps land
    /// under the call that launched it, never inside the parent's reply, and
    /// its steps never split that reply while the parent keeps writing.
    /// W1-T: a native runtime's thinking lands as one stretch, with how long
    /// it took (from the turn's start) and its text.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_runtimes_thinking_lands_before_its_answer_with_its_time() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipethought";
        let workspace = tempfile::tempdir().unwrap();
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![env("BAKERY_FAKE_ACP_THOUGHT", "Let me weigh it."), env("BAKERY_FAKE_ACP_MESSAGE", "Done.")],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "claude-native".to_string(),
        );
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-1");
        model.set_current_turn_id("turn-1");
        assert_eq!(model.chat("system", &[ChatMsg::user("think first")]).unwrap(), "Done.");

        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let fact = events.iter().find(|e| e.family == THOUGHT_FAMILY).expect("a stretch of thinking");
        let thought: serde_json::Value = serde_json::from_str(&fact.payload).unwrap();
        assert_eq!((thought["text"].as_str(), thought["shared"].as_bool()), (Some("Let me weigh it."), Some(true)));
        assert!(thought["ms"].as_u64().is_some_and(|ms| ms >= 30), "{thought}");
        assert_eq!(thought["turn_id"], "turn-1");
        assert_eq!(events.iter().filter(|e| e.family == THOUGHT_FAMILY).count(), 1, "one stretch");
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_subagents_work_lands_under_its_call_and_never_splits_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipesubagent";
        let workspace = tempfile::tempdir().unwrap();
        let env = |k: &str, v: &str| (k.to_string(), v.to_string());
        let model = AcpPipeModel::new(
            state.clone(),
            stream_id.to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![env("BAKERY_FAKE_ACP_SUBAGENT", "1"), env("BAKERY_FAKE_ACP_MESSAGE", "Done.")],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "claude-native".to_string(),
        );
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let _cancel = crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn-1");
        model.set_current_turn_id("turn-1");

        let reply = model.chat("system", &[ChatMsg::user("read the refs")]).unwrap();
        assert_eq!(reply, "I'll ask a helper.\n\nMeanwhile the answer goes on in one piece. Done.");

        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let order: Vec<String> = events
            .iter()
            .filter_map(|e| {
                let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
                match e.family.as_str() {
                    ACP_TEXT_EVENT_FAMILY => Some(format!("text:{}", p["text"].as_str().unwrap())),
                    ACP_SUBAGENT_TEXT_FAMILY => Some(format!(
                        "{} says:{}",
                        p["parent_tool_call_id"].as_str().unwrap(),
                        p["text"].as_str().unwrap()
                    )),
                    ACP_TOOL_EVENT_FAMILY => Some(format!(
                        "{}:{}",
                        p["event"].as_str().unwrap(),
                        p["detail"]["toolCallId"].as_str().unwrap()
                    )),
                    _ => None,
                }
            })
            .collect();
        assert_eq!(
            order,
            [
                "text:I'll ask a helper.",
                "tool_call:agent-1",
                "agent-1 says:Looking at README.",
                "tool_call:sub-read-1",
                "tool_call_update:sub-read-1",
                "agent-1 says:Found it.",
                "tool_call_update:agent-1",
                "text:Meanwhile the answer goes on in one piece. Done.",
            ]
        );
        let child = events
            .iter()
            .find(|e| e.family == ACP_TOOL_EVENT_FAMILY && e.payload.contains("sub-read-1"))
            .unwrap();
        let child: serde_json::Value = serde_json::from_str(&child.payload).unwrap();
        assert_eq!(child["detail"]["_meta"]["claudeCode"]["parentToolUseId"], "agent-1");
    }

    /// Claude's AskUserQuestion reaches the human: the adapter's form
    /// elicitation becomes the panel's questions, the turn parks on them, and
    /// the structured answer goes back as the form's content.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn native_questions_park_the_turn_and_return_the_answers() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "native-ask"))
            .unwrap()
            .context
            .stream_id;
        let agent_id = crate::node::agent_id(&stream_id, Seat::Orchestrator);
        let model = Arc::new(fake_model(
            &state,
            &stream_id,
            vec![("BAKERY_FAKE_ACP_ELICIT".to_string(), "1".to_string())],
        ));
        let _cancel = crate::state::start_agent_activity(&state, &stream_id, &agent_id, "ask-turn");
        model.set_current_turn_id("ask-turn");
        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("system", &[ChatMsg::user("ask me")]))
        };

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let asked = loop {
            let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
            if let Some(event) = events.into_iter().find(|e| e.family == "agent.ask.pending") {
                break event;
            }
            assert!(std::time::Instant::now() < deadline, "the native question never reached Heart");
            std::thread::sleep(Duration::from_millis(20));
        };
        let p: serde_json::Value = serde_json::from_str(&asked.payload).unwrap();
        assert_eq!(p["questions"][0]["header"], "Branch");
        assert_eq!(p["questions"][0]["options"][1]["description"], "staging");
        assert_eq!(p["questions"][1]["multiSelect"], true);

        crate::routes::agent::agent_answer_inner(
            &state,
            &crate::routes::agent::AgentAnswerReq {
                stream_id: stream_id.clone(),
                event_hash: asked.event_hash.clone(),
                answer: String::new(),
                answers: Some(vec![
                    crate::ask::AskAnswer { picks: vec!["dev".into()], other: None },
                    crate::ask::AskAnswer { picks: vec!["docs".into()], other: Some("and a demo".into()) },
                ]),
            },
        )
        .unwrap();
        let reply = running.join().unwrap().unwrap();
        for part in [
            r#""action":"accept""#,
            r#""question_0":"dev""#,
            r#""question_1":["docs"]"#,
            r#""question_1_custom":"and a demo""#,
        ] {
            assert!(reply.contains(part), "{part} missing from {reply}");
        }
    }

    /// Text outside a live prompt is never recorded; every flush in a turn
    /// records its new text, however many there are, and nothing twice.
    #[test]
    fn text_flushes_record_only_live_new_text_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipetextflush";
        let text = Mutex::new("already said".to_string());
        let flushed = AtomicUsize::new(0);
        let flush = |turn_id: &str, last: bool| {
            flush_acp_text(
                &state,
                stream_id,
                Seat::Orchestrator,
                "acp:x",
                turn_id,
                &text,
                &flushed,
                last,
            )
        };
        let texts = || -> Vec<String> {
            state
                .heart
                .lock()
                .unwrap()
                .read_stream(stream_id)
                .unwrap()
                .iter()
                .filter(|e| e.family == ACP_TEXT_EVENT_FAMILY)
                .map(|e| {
                    let p: serde_json::Value = serde_json::from_str(&e.payload).unwrap();
                    p["text"].as_str().unwrap().to_string()
                })
                .collect()
        };

        flush("", false);
        flush("", true);
        assert!(texts().is_empty(), "no live prompt, nothing recorded");

        flush("t1", false);
        assert_eq!(texts(), ["already said"], "however many came before it");
        text.lock().unwrap().push_str("and more");
        flush("t1", true);
        flush("t1", true);
        assert_eq!(texts(), ["already said", "and more"], "the last flush records the rest, once");
    }

    /// A diff string past the per-string bound is cut in the Heart's copy,
    /// and the whole payload is kept beside it, linked from the record.
    #[test]
    fn a_long_diff_is_kept_whole_beside_its_record() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipebigdiff-seq";
        let big = "y".repeat(MAX_ACP_TOOL_EVENT_STRING * 2);
        let tool_call = serde_json::json!({"toolCallId": "tc-1", "title": "t", "content": [{"type": "diff", "path": "/x", "oldText": big, "newText": "short"}]});
        record_acp_tool_event(
            &state,
            stream_id,
            Seat::Orchestrator,
            "acp:x",
            "t1",
            "tool_call",
            &tool_call,
        );

        let events = acp_tool_call_events(&state, stream_id);
        assert_eq!(events.len(), 1);
        let old_text = events[0]["detail"]["content"][0]["oldText"]
            .as_str()
            .unwrap();
        assert!(
            old_text.len() < big.len(),
            "must be truncated: {} vs {}",
            old_text.len(),
            big.len()
        );
        assert!(old_text.contains("truncated"));
        let sha = events[0]["full"]["sha"].as_str().unwrap();
        let whole = read_full_tool_payload(&state, stream_id, sha).unwrap();
        assert!(whole.contains(&big), "every byte of it");
    }

    /// Every tool notification of a turn is recorded, however many there
    /// are.
    #[test]
    fn every_tool_call_event_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipetoolcap";
        for i in 0..1_000 {
            let tool_call = serde_json::json!({"toolCallId": format!("tc-{i}"), "title": "t"});
            record_acp_tool_event(
                &state,
                stream_id,
                Seat::Orchestrator,
                "acp:x",
                "t1",
                "tool_call",
                &tool_call,
            );
        }
        assert_eq!(acp_tool_call_events(&state, stream_id).len(), 1_000);
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn ambiguous_agent_auth_methods_fail_closed_with_a_setup_message() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(
            &state,
            "wt:pipenoauth",
            vec![(
                "BAKERY_FAKE_ACP_AMBIGUOUS_AUTH".to_string(),
                "1".to_string(),
            )],
        );

        let err = model.chat("system", &[ChatMsg::user("hi")]).unwrap_err();
        assert!(err.contains("Setup needed"), "{err}");
        assert!(err.contains("sign in"), "{err}");
    }

    /// Schema v1 dropped the `env_var` auth type, so the typed response reads
    /// such a method as agent-managed. The way Codex advertises its API keys
    /// must still show them as client credentials and sign in with ChatGPT.
    #[test]
    fn env_var_auth_methods_stay_client_credentials() {
        let raw = serde_json::json!({
            "protocolVersion": 1,
            "authMethods": [
                {"id": "chatgpt", "name": "Login with ChatGPT"},
                {"id": "codex-api-key", "name": "Use CODEX_API_KEY", "type": "env_var", "vars": [{"name": "CODEX_API_KEY"}]},
                {"id": "openai-api-key", "name": "Use OPENAI_API_KEY", "type": "env_var", "vars": [{"name": "OPENAI_API_KEY"}]},
            ],
        });
        let init: acp::InitializeResponse = serde_json::from_value(raw.clone()).unwrap();
        let env_var_methods = env_var_auth_methods(&raw);
        let kinds: Vec<_> = init
            .auth_methods
            .iter()
            .map(|method| project_auth_method(method, &env_var_methods).kind)
            .collect();
        assert_eq!(kinds, ["agent", "environment", "environment"]);
        let selected = select_agent_auth_method(&init, &env_var_methods).unwrap();
        assert_eq!(selected.unwrap().0.as_ref(), "chatgpt");
    }

    // ─── 07d-A3B: named ACP plugs ─────────────────────────────────────────────

    /// Proof: two distinct named ACP plug ids (`acp:alpha`, `acp:beta`),
    /// each independently resolved via `state::resolve_acp_plug_config` and
    /// each completing its own fake-agent round trip, on a workspace that is
    /// deliberately not a git repo at all (07d-A3B Proof: "two ACP plug ids;
    /// fake agent round-trip on a non-git A3 project").
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn two_named_acp_plugs_boot_and_answer_independently_on_a_non_git_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let workspace = tempfile::tempdir().unwrap();
        assert!(
            !workspace.path().join(".git").exists(),
            "workspace must not be a git repo"
        );

        {
            let mut node = state.node.lock().unwrap();
            node.acp_plugs = vec![
                crate::node::AcpPlugConfig {
                    id: "acp:alpha".into(),
                    label: Some("Alpha CLI".into()),
                    argv: vec![fake_acp_agent_binary().to_string_lossy().into_owned()],
                    config: None,
                    env: Default::default(),
                    mcp_transport: crate::node::default_mcp_transport(),
                },
                crate::node::AcpPlugConfig {
                    id: "acp:beta".into(),
                    label: Some("Beta CLI".into()),
                    argv: vec![fake_acp_agent_binary().to_string_lossy().into_owned()],
                    config: None,
                    env: Default::default(),
                    mcp_transport: crate::node::default_mcp_transport(),
                },
            ];
        }

        let (alpha_binary, alpha_argv, alpha_config) =
            crate::state::resolve_acp_plug_config(&state, "acp:alpha").unwrap();
        let (beta_binary, beta_argv, beta_config) =
            crate::state::resolve_acp_plug_config(&state, "acp:beta").unwrap();

        let alpha = AcpPipeModel::new(
            state.clone(),
            "wt:alpha-stream".to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: alpha_binary,
                argv_suffix: alpha_argv,
                config_copy: alpha_config,
                extra_env: vec![(
                    "BAKERY_FAKE_ACP_MESSAGE".to_string(),
                    "hello from alpha".to_string(),
                )],
                trusted: false,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp:alpha".to_string(),
        );
        let beta = AcpPipeModel::new(
            state.clone(),
            "wt:beta-stream".to_string(),
            Seat::Orchestrator,
            workspace.path().to_path_buf(),
            AcpSpawnConfig {
                binary: beta_binary,
                argv_suffix: beta_argv,
                config_copy: beta_config,
                extra_env: vec![(
                    "BAKERY_FAKE_ACP_MESSAGE".to_string(),
                    "hello from beta".to_string(),
                )],
                trusted: false,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp:beta".to_string(),
        );

        let alpha_reply = alpha.chat("system", &[ChatMsg::user("hi")]).unwrap();
        let beta_reply = beta.chat("system", &[ChatMsg::user("hi")]).unwrap();

        assert_eq!(alpha_reply, "hello from alpha");
        assert_eq!(beta_reply, "hello from beta");
        assert_eq!(alpha.name(), "acp:alpha");
        assert_eq!(beta.name(), "acp:beta");
        assert_eq!(alpha.transport(), bakery_agent::Transport::Acp);
    }

    /// Proof: a named ACP plug pointed at a binary that does not exist on
    /// disk fails closed as "Setup needed" and never echoes the configured
    /// path — same sanitization discipline as the legacy singular row
    /// (`routes::studio::acp_cli_pointing_at_a_nonexistent_binary_is_node_unavailable`).
    #[test]
    fn resolve_acp_plug_config_for_a_missing_binary_fails_setup_without_leaking_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let missing = dir.path().join("does-not-exist-acp-cli");
        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:codex".into(),
            label: None,
            argv: vec![missing.to_string_lossy().into_owned()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];

        let err = crate::state::resolve_acp_plug_config(&state, "acp:codex")
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_ascii_lowercase().contains("setup needed"), "{err}");
        assert!(
            !err.contains(&missing.to_string_lossy().into_owned()),
            "must never echo the configured path: {err}"
        );

        let unknown_err = crate::state::resolve_acp_plug_config(&state, "acp:nonexistent-plug")
            .map(|_| ())
            .unwrap_err();
        assert!(
            unknown_err.to_ascii_lowercase().contains("setup needed"),
            "{unknown_err}"
        );
    }

    #[test]
    fn acp_cursor_missing_binary_is_setup() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:cursor".into(),
            label: Some("Cursor".into()),
            // A program no Mac has: the test is about a missing one, not
            // about whether this Mac has Cursor installed.
            argv: vec!["cursor-agent-never-installed".into(), "acp".into()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        let err = crate::state::resolve_acp_plug_config(&state, "acp:cursor")
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_ascii_lowercase().contains("setup needed"), "{err}");
        assert!(
            !err.contains("cursor-agent"),
            "must not echo the configured binary: {err}"
        );
    }

    #[test]
    fn acp_plug_extra_env_passes_allowlisted_directory_vars() {
        let mut env = std::collections::HashMap::new();
        env.insert("AGY_BIN".into(), "/tmp/agy".into());
        env.insert("CODEX_HOME".into(), "/tmp/codex-home".into());
        let plug = crate::node::AcpPlugConfig {
            id: "acp:agy".into(),
            label: None,
            argv: vec!["agy-acp".into()],
            config: None,
            env,
            mcp_transport: crate::node::default_mcp_transport(),
        };
        let extra = crate::state::acp_plug_extra_env(&plug).unwrap();
        assert!(extra.contains(&("AGY_BIN".into(), "/tmp/agy".into())));
        assert!(extra.contains(&("CODEX_HOME".into(), "/tmp/codex-home".into())));
    }

    #[test]
    fn acp_plug_env_allowlist_rejects_unknown_keys() {
        let mut env = std::collections::HashMap::new();
        env.insert("SECRET_TOKEN".into(), "nope".into());
        let err = crate::state::validate_acp_plug_env(&env).unwrap_err();
        assert!(err.to_ascii_lowercase().contains("setup needed"), "{err}");
        let mut ok = std::collections::HashMap::new();
        ok.insert("AGY_BIN".into(), "/tmp/agy".into());
        assert!(crate::state::validate_acp_plug_env(&ok).is_ok());
    }

    /// Proof: a bare executable name (`argv[0]` with no path separator, e.g.
    /// `"codex"` or `"claude"`) resolves through `PATH` the same way
    /// `std::process::Command`/`execvp` would — 07d-A3B review #5:
    /// previously only an absolute (or cwd-relative) path ever matched.
    #[test]
    fn resolve_acp_plug_config_resolves_a_bare_name_via_path() {
        static PATH_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = PATH_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let bin_dir = dir.path().join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let fake_cli = bin_dir.join("my-fake-acp-cli");
        std::fs::write(&fake_cli, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let prev_path = std::env::var("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(bin_dir.clone()).chain(std::env::split_paths(&prev_path)),
        )
        .unwrap();
        std::env::set_var("PATH", &new_path);

        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:bare".into(),
            label: None,
            argv: vec!["my-fake-acp-cli".into()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        let result = crate::state::resolve_acp_plug_config(&state, "acp:bare");

        std::env::set_var("PATH", &prev_path);

        let (binary, _, _) = result.unwrap();
        assert_eq!(
            binary, fake_cli,
            "a bare name must resolve via PATH to the exact staged file"
        );
    }

    /// A bare name that names a path-separator-free but still not-on-PATH
    /// binary fails the same sanitized way a missing absolute path does —
    /// never echoing PATH's own contents (which can carry other software's
    /// layout) any more than it echoes the configured value.
    #[test]
    fn resolve_acp_plug_config_bare_name_not_on_path_fails_setup() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().acp_plugs = vec![crate::node::AcpPlugConfig {
            id: "acp:missing-bare".into(),
            label: None,
            argv: vec!["definitely-not-a-real-binary-anywhere-on-path".into()],
            config: None,
            env: Default::default(),
            mcp_transport: crate::node::default_mcp_transport(),
        }];
        let err = crate::state::resolve_acp_plug_config(&state, "acp:missing-bare")
            .map(|_| ())
            .unwrap_err();
        assert!(err.to_ascii_lowercase().contains("setup needed"), "{err}");
    }

    // ─── Gate logic (no ACP process) ─────────────────────────────────────────

    fn req(kind: ChildToolKind) -> ChildToolRequest {
        ChildToolRequest {
            plug: "test".to_string(),
            kind,
            title: "a test tool call".to_string(),
            diffs: Vec::new(),
        }
    }

    #[test]
    fn auto_safe_kinds_are_always_allowed_and_never_gated() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        for kind in [
            ChildToolKind::Read,
            ChildToolKind::Search,
            ChildToolKind::Think,
            ChildToolKind::Fetch,
        ] {
            assert!(matches!(
                admit_child_tool_call(&state, "wt:gate", Seat::Orchestrator, "t1", &req(kind)),
                ChildToolDecision::Allow
            ));
        }
        assert!(pending_gate_events(&state, "wt:gate").is_empty());
    }

    #[test]
    fn a_gated_kind_is_denied_and_recorded_exactly_once_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:gate2",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        assert_eq!(pending_gate_events(&state, "wt:gate2").len(), 1);
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:gate2",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        assert_eq!(pending_gate_events(&state, "wt:gate2").len(), 2);
    }

    /// 07d-A5 review #5: a gate request carrying a `turn_id` that no longer
    /// matches the agent's CURRENT tracked activity (an abandoned ACP
    /// session's request landing after a newer turn started) must still be
    /// recorded (the Heart event and the `Deny` return are both
    /// unconditional — a real gate genuinely needs deciding regardless) but
    /// must never clobber that newer turn's real, current phase.
    #[test]
    fn a_stale_turn_ids_gate_request_never_clobbers_a_newer_turns_activity_phase() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:gate-stale";
        let agent_id = crate::node::agent_id(stream_id, Seat::Worker);
        crate::state::start_agent_activity(&state, stream_id, &agent_id, "turn_current");

        // A request tagged with a DIFFERENT (stale/abandoned) turn_id.
        assert!(matches!(
            admit_child_tool_call(
                &state,
                stream_id,
                Seat::Worker,
                "turn_stale",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        assert_eq!(
            pending_gate_events(&state, stream_id).len(),
            1,
            "the gate event itself is still recorded"
        );
        assert!(
            matches!(
                crate::state::agent_activity_snapshot(&state, &agent_id)
                    .unwrap()
                    .phase,
                crate::state::ActivityPhase::ContactingModel
            ),
            "a stale turn_id must never clobber the current turn's real phase"
        );

        // The SAME turn_id as the currently tracked activity — must set
        // AwaitingApproval normally.
        assert!(matches!(
            admit_child_tool_call(
                &state,
                stream_id,
                Seat::Worker,
                "turn_current",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        assert!(matches!(
            crate::state::agent_activity_snapshot(&state, &agent_id)
                .unwrap()
                .phase,
            crate::state::ActivityPhase::AwaitingApproval { .. }
        ));
    }

    #[test]
    fn allow_once_is_consumed_by_exactly_the_next_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state
            .child_tool_allow_once
            .lock()
            .unwrap()
            .insert(("wt:gate3".to_string(), ChildToolKind::Execute));

        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:gate3",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Allow
        ));
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:gate3",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
    }

    #[test]
    fn allow_always_in_node_config_allows_every_future_call_of_that_kind() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state
            .node
            .lock()
            .unwrap()
            .always_kinds
            .push(ChildToolKind::Execute);

        for _ in 0..3 {
            assert!(matches!(
                admit_child_tool_call(
                    &state,
                    "wt:gate4",
                    Seat::Worker,
                    "t1",
                    &req(ChildToolKind::Execute)
                ),
                ChildToolDecision::Allow
            ));
        }
        assert!(pending_gate_events(&state, "wt:gate4").is_empty());
    }

    // ─── Deciding a pending gate ──────────────────────────────────────────────

    fn open_pending_gate(state: &AppState, stream_id: &str, kind: ChildToolKind) -> String {
        // A real gate is raised by a live turn; one without it has already
        // expired (see `gate_turn_has_ended`).
        let agent_id = crate::node::agent_id(stream_id, Seat::Worker);
        crate::state::start_agent_activity(state, stream_id, &agent_id, "t1");
        assert!(matches!(
            admit_child_tool_call(state, stream_id, Seat::Worker, "t1", &req(kind)),
            ChildToolDecision::Deny
        ));
        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        events
            .iter()
            .rev()
            .find(|e| e.family == CHILD_TOOL_PENDING_FAMILY)
            .unwrap()
            .event_hash
            .clone()
    }

    #[test]
    fn deciding_allow_always_persists_to_node_config_and_a_later_call_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let hash = open_pending_gate(&state, "wt:decide1", ChildToolKind::Execute);

        decide_child_tool_gate(
            &state,
            "wt:decide1",
            &hash,
            ChildToolGateDecisionKind::AllowAlways,
        )
        .unwrap();

        // A chat in no project: the grant is that chat's alone.
        let grant = AlwaysGrant { scope: GrantScope::Chat, id: Some("wt:decide1".into()), kind: ChildToolKind::Execute };
        assert_eq!(state.node.lock().unwrap().always_grants, vec![grant.clone()]);
        assert!(state.node.lock().unwrap().always_kinds.is_empty(), "Settings' rules are untouched");
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:decide1",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Allow
        ));

        assert!(matches!(
            admit_child_tool_call(&state, "wt:another-chat", Seat::Worker, "t1", &req(ChildToolKind::Execute)),
            ChildToolDecision::Deny
        ), "another chat still asks");

        let reloaded = crate::node::NodeConfig::load_or_create(dir.path());
        assert_eq!(reloaded.always_grants, vec![grant]);
    }

    /// An "Always allow" holds where its card said: in every conversation of
    /// its project and no other project's, or in the Baker's conversations
    /// and no other agent's.
    #[test]
    fn allow_always_holds_in_its_project_or_in_the_bakers_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (p, p1) = crate::catalog::create_project(&state, "thebakery", None).unwrap();
        let p2 = crate::routes::project_catalog::open_root(
            &state,
            &crate::routes::project_catalog::OpenRootReq {
                project_id: p.project_id.clone(),
                name: "second".into(),
                root: crate::routes::project_catalog::CreateProjectRoot::Create {
                    parent: dir.path().to_string_lossy().into_owned(),
                    name: "second".into(),
                },
                tools: None,
            },
        )
        .unwrap();
        let (_, q1) = crate::catalog::create_project(&state, "other", None).unwrap();
        let (p1, p2, q1) = (p1.stream_id, p2.stream_id, q1.stream_id);
        let admit = |stream: &str, kind| admit_child_tool_call(&state, stream, Seat::Worker, "t1", &req(kind));

        let hash = open_pending_gate(&state, &p1, ChildToolKind::Execute);
        let events = state.heart.lock().unwrap().read_stream(&p1).unwrap();
        let card: serde_json::Value =
            serde_json::from_str(&events.iter().find(|e| e.event_hash == hash).unwrap().payload).unwrap();
        let offered = serde_json::json!({"scope": "project", "id": p.project_id, "kind": "execute", "name": "thebakery"});
        assert_eq!(card["always"], offered, "the card names what it grants");
        decide_child_tool_gate(&state, &p1, &hash, ChildToolGateDecisionKind::AllowAlways).unwrap();
        assert!(matches!(admit(&p2, ChildToolKind::Execute), ChildToolDecision::Allow), "all of its project");
        assert!(matches!(admit(&q1, ChildToolKind::Execute), ChildToolDecision::Deny), "no other project");

        state.node.lock().unwrap().baker_stream_id = Some("wt:baker".to_string());
        let hash = open_pending_gate(&state, "wt:baker", ChildToolKind::Other);
        decide_child_tool_gate(&state, "wt:baker", &hash, ChildToolGateDecisionKind::AllowAlways).unwrap();
        assert_eq!(state.node.lock().unwrap().always_grants[1].scope, GrantScope::Baker);
        assert!(matches!(admit("wt:baker", ChildToolKind::Other), ChildToolDecision::Allow));
        assert!(matches!(admit(&p1, ChildToolKind::Other), ChildToolDecision::Deny), "no other agent");
    }

    #[test]
    fn deciding_allow_once_pre_clears_exactly_one_future_call_on_that_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let hash = open_pending_gate(&state, "wt:decide2", ChildToolKind::Execute);

        decide_child_tool_gate(
            &state,
            "wt:decide2",
            &hash,
            ChildToolGateDecisionKind::AllowOnce,
        )
        .unwrap();

        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:decide2",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Allow
        ));
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:decide2",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        assert!(!state
            .node
            .lock()
            .unwrap()
            .always_kinds
            .contains(&ChildToolKind::Execute));
    }

    #[test]
    fn deciding_deny_changes_nothing_but_records_the_decision() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let hash = open_pending_gate(&state, "wt:decide3", ChildToolKind::Execute);

        let ev =
            decide_child_tool_gate(&state, "wt:decide3", &hash, ChildToolGateDecisionKind::Deny)
                .unwrap();
        assert_eq!(ev.family, CHILD_TOOL_DECIDED_FAMILY);
        assert_eq!(ev.causality, vec![hash]);
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:decide3",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
    }

    #[test]
    fn deciding_the_same_gate_concurrently_lets_exactly_one_decision_win() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let hash = open_pending_gate(&state, "wt:decide-race", ChildToolKind::Execute);

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = [
            ChildToolGateDecisionKind::AllowAlways,
            ChildToolGateDecisionKind::Deny,
        ]
        .into_iter()
        .map(|decision| {
            let state = state.clone();
            let hash = hash.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                decide_child_tool_gate(&state, "wt:decide-race", &hash, decision)
            })
        })
        .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let oks = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(
            oks, 1,
            "exactly one concurrent decision must win: {results:?}"
        );
        let err = results.iter().find_map(|r| r.as_ref().err()).unwrap();
        assert!(err.contains("already has a gate decision"), "{err}");
    }

    /// On a NON-task socket a workspace mutation is not a gate at all: the
    /// node records the request and its own deny in one stroke, so no human
    /// is ever shown an approval they structurally cannot grant (the old
    /// behavior surfaced exactly that phantom gate).
    #[test]
    fn edit_delete_move_on_a_plain_socket_are_denied_decidedly_not_left_as_a_phantom_gate() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        for kind in [
            ChildToolKind::Edit,
            ChildToolKind::Delete,
            ChildToolKind::Move,
        ] {
            let stream_id = format!("wt:noapprove-{kind:?}");
            assert!(matches!(
                admit_child_tool_call(&state, &stream_id, Seat::Worker, "t1", &req(kind)),
                ChildToolDecision::Deny
            ));
            let events = state.heart.lock().unwrap().read_stream(&stream_id).unwrap();
            let pending = events
                .iter()
                .find(|e| e.family == CHILD_TOOL_PENDING_FAMILY)
                .unwrap();
            let decided = events
                .iter()
                .find(|e| e.family == CHILD_TOOL_DECIDED_FAMILY)
                .expect("the node must decide its own deny immediately");
            assert_eq!(decided.actor, "node");
            assert!(decided.causality.contains(&pending.event_hash));

            // A human decision arriving later finds it already decided —
            // and an allow would have been refused anyway.
            let err = decide_child_tool_gate(
                &state,
                &stream_id,
                &pending.event_hash,
                ChildToolGateDecisionKind::AllowAlways,
            )
            .unwrap_err();
            assert!(err.contains("already has a gate decision"), "{err}");
            assert!(!state
                .node
                .lock()
                .unwrap()
                .always_kinds
                .contains(&kind));
        }
    }

    /// A runtime that shows its exact change (ACP diff content) can be
    /// approved on any socket, in Manual, like a proposal: the gate carries
    /// the diff for the operator to read, and an allow admits it once.
    #[test]
    fn an_edit_that_shows_its_diff_is_approvable_on_a_plain_socket() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:shows-diff";
        let agent_id = crate::node::agent_id(stream_id, Seat::Worker);
        crate::state::start_agent_activity(&state, stream_id, &agent_id, "t1");
        let mut edit = req(ChildToolKind::Edit);
        edit.diffs = vec![serde_json::json!({
            "path": "src/main.rs", "old_text": "a\n", "new_text": "b\n",
        })];
        let admission =
            admit_child_tool_call_detailed(&state, stream_id, Seat::Worker, "t1", &edit, true);
        let ChildToolAdmission::Pending(hash) = admission else {
            panic!("a visible change is a real gate, not a refusal");
        };
        let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
        let pending = events.iter().find(|e| e.event_hash == hash).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&pending.payload).unwrap();
        assert_eq!(payload["diffs"][0]["new_text"], "b\n");

        decide_child_tool_gate(&state, stream_id, &hash, ChildToolGateDecisionKind::AllowOnce)
            .unwrap();
        assert!(matches!(
            admit_child_tool_call_detailed(&state, stream_id, Seat::Worker, "t1", &edit, true),
            ChildToolAdmission::Allow
        ));
    }

    /// On a TASK socket the same kinds are approvable like Execute: the
    /// task's baseline Review diff is the review surface, so a native
    /// worker may edit once the human allows it — capability inside the
    /// sandbox instead of a permanently undecidable refusal.
    #[test]
    fn edit_is_approvable_on_a_task_socket_and_allow_once_admits_the_next_call() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let created = crate::catalog::create_project(
            &state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .unwrap();
        let task = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: created.0.project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        let stream_id = task.task.stream_id.clone();

        // First call: pending gate (a real one), denied for now.
        let hash = open_pending_gate(&state, &stream_id, ChildToolKind::Edit);
        decide_child_tool_gate(
            &state,
            &stream_id,
            &hash,
            ChildToolGateDecisionKind::AllowOnce,
        )
        .unwrap();
        assert!(matches!(
            admit_child_tool_call(
                &state,
                &stream_id,
                Seat::Worker,
                "t2",
                &req(ChildToolKind::Edit)
            ),
            ChildToolDecision::Allow
        ));
        // Once means once.
        assert!(matches!(
            admit_child_tool_call(
                &state,
                &stream_id,
                Seat::Worker,
                "t3",
                &req(ChildToolKind::Edit)
            ),
            ChildToolDecision::Deny
        ));
    }

    /// An AllowAlways granted on a task socket must not leak to plain
    /// sockets through the node-global list — the guard consults the list
    /// only where the kind is approvable at all.
    #[test]
    fn allow_always_for_edit_on_a_task_socket_does_not_open_plain_sockets() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let repo = make_repo(&dir.path().join("repo"));
        let created = crate::catalog::create_project(
            &state,
            "P",
            Some(crate::catalog::DirectoryRootSpec::Attach(
                &repo.to_string_lossy(),
            )),
        )
        .unwrap();
        let task = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: created.0.project_id.clone(),
                title: "T".into(),
                work_order: "wo".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        let stream_id = task.task.stream_id.clone();
        let hash = open_pending_gate(&state, &stream_id, ChildToolKind::Edit);
        decide_child_tool_gate(
            &state,
            &stream_id,
            &hash,
            ChildToolGateDecisionKind::AllowAlways,
        )
        .unwrap();
        let grant = AlwaysGrant { scope: GrantScope::Project, id: Some(created.0.project_id.clone()), kind: ChildToolKind::Edit };
        assert_eq!(state.node.lock().unwrap().always_grants, vec![grant], "its project's");

        // The task socket is now allowed…
        assert!(matches!(
            admit_child_tool_call(
                &state,
                &stream_id,
                Seat::Worker,
                "t2",
                &req(ChildToolKind::Edit)
            ),
            ChildToolDecision::Allow
        ));
        // …but a plain socket still is not.
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:plain-still-denied",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Edit)
            ),
            ChildToolDecision::Deny
        ));
    }

    #[test]
    fn admit_refuses_edit_delete_move_even_if_somehow_already_in_the_allow_list() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state
            .node
            .lock()
            .unwrap()
            .always_kinds
            .push(ChildToolKind::Edit);
        state
            .child_tool_allow_once
            .lock()
            .unwrap()
            .insert(("wt:sneaky".to_string(), ChildToolKind::Delete));

        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:sneaky",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Edit)
            ),
            ChildToolDecision::Deny
        ));
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:sneaky",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Delete)
            ),
            ChildToolDecision::Deny
        ));
    }

    #[test]
    fn deciding_an_already_decided_gate_twice_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let hash = open_pending_gate(&state, "wt:decide4", ChildToolKind::Execute);

        decide_child_tool_gate(&state, "wt:decide4", &hash, ChildToolGateDecisionKind::Deny)
            .unwrap();
        let err = decide_child_tool_gate(
            &state,
            "wt:decide4",
            &hash,
            ChildToolGateDecisionKind::AllowAlways,
        )
        .unwrap_err();
        assert!(err.contains("already has a gate decision"), "{err}");
        assert!(!state
            .node
            .lock()
            .unwrap()
            .always_kinds
            .contains(&ChildToolKind::Execute));
    }

    #[test]
    fn deciding_an_unknown_hash_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = decide_child_tool_gate(
            &state,
            "wt:decide5",
            "not-a-real-hash",
            ChildToolGateDecisionKind::Deny,
        )
        .unwrap_err();
        assert!(err.contains("no event"), "{err}");
    }

    /// Nothing is parked on a gate whose turn is over (cancelled, or lost to
    /// a node restart), so an Allow on it must be refused — recording it
    /// would leave an allow-once behind that silently pre-approves the next,
    /// unrelated call of the same kind.
    #[test]
    fn deciding_a_gate_whose_turn_ended_is_refused_and_leaves_no_allowance() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let ended = open_pending_gate(&state, "wt:expired1", ChildToolKind::Execute);
        crate::state::append(
            &state,
            "wt:expired1",
            "node",
            "agent.turn.cancelled",
            serde_json::json!({ "turn_id": "t1" }).to_string(),
            vec![],
        )
        .unwrap();

        // No activity at all: what a node restart leaves behind.
        assert!(matches!(
            admit_child_tool_call(
                &state,
                "wt:expired2",
                Seat::Worker,
                "t1",
                &req(ChildToolKind::Execute)
            ),
            ChildToolDecision::Deny
        ));
        let orphan = state
            .heart
            .lock()
            .unwrap()
            .read_stream("wt:expired2")
            .unwrap()
            .into_iter()
            .find(|e| e.family == CHILD_TOOL_PENDING_FAMILY)
            .unwrap()
            .event_hash;

        for (stream_id, hash) in [("wt:expired1", ended), ("wt:expired2", orphan)] {
            let err = decide_child_tool_gate(
                &state,
                stream_id,
                &hash,
                ChildToolGateDecisionKind::AllowOnce,
            )
            .unwrap_err();
            assert!(err.contains("expired"), "{err}");
            let events = state.heart.lock().unwrap().read_stream(stream_id).unwrap();
            assert!(!events.iter().any(|e| e.family == CHILD_TOOL_DECIDED_FAMILY));
        }
        assert!(state.child_tool_allow_once.lock().unwrap().is_empty());
    }

    // ─── 06c-A2 proofs ───────────────────────────────────────────────────────

    /// Second chat() on the same (stream, seat) reuses the same pid — the
    /// session is persistent, not re-spawned per turn.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn second_chat_reuses_same_pid() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = fake_model(&state, "wt:pipe-persist", Vec::new());

        let _r1 = model.chat("sys", &[ChatMsg::user("hi1")]).unwrap();
        let pid1 = {
            let sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&("wt:pipe-persist".to_string(), Seat::Orchestrator)) {
                Some(AcpSlot::Live(live)) => live.pid,
                _ => panic!("session must be Live after first chat"),
            }
        };

        let _r2 = model.chat("sys", &[ChatMsg::user("hi2")]).unwrap();
        let pid2 = {
            let sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&("wt:pipe-persist".to_string(), Seat::Orchestrator)) {
                Some(AcpSlot::Live(live)) => live.pid,
                _ => panic!("session must be Live after second chat"),
            }
        };

        assert_eq!(
            pid1, pid2,
            "second chat must reuse the same session (same pid)"
        );
    }

    /// Two threads racing on the first chat() must produce at most one child.
    /// One of the two may get a "retry shortly" error; at least one must succeed.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn two_concurrent_first_chats_do_not_double_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-conc";
        let model = Arc::new(fake_model(&state, stream_id, Vec::new()));

        let barrier = Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let model = model.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    model.chat("sys", &[ChatMsg::user("hi")])
                })
            })
            .collect();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

        let ok_count = results.iter().filter(|r| r.is_ok()).count();
        assert!(
            ok_count >= 1,
            "at least one concurrent chat must succeed: {results:?}"
        );

        let sessions = state.acp_sessions.lock().unwrap();
        let live_pids: Vec<u32> = sessions
            .values()
            .filter_map(|s| {
                if let AcpSlot::Live(l) = s {
                    Some(l.pid)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            live_pids.len() <= 1,
            "at most one child process must be spawned: {live_pids:?}"
        );
    }

    #[test]
    fn frame_limiter_resets_at_each_newline_not_at_each_turn() {
        const CAP: usize = 4096;
        let frame = vec![b'x'; CAP];
        let mut input = Vec::with_capacity((CAP + 1) * 3);
        for _ in 0..3 {
            input.extend_from_slice(&frame);
            input.push(b'\n');
        }
        let mut reader = FrameLimitedAsyncRead {
            inner: futures::io::Cursor::new(input.clone()),
            remaining: CAP,
            cap: CAP,
        };
        let mut output = Vec::new();
        futures::executor::block_on(reader.read_to_end(&mut output)).unwrap();
        assert_eq!(output, input, "three individually bounded frames must survive even though their aggregate exceeds the cap");
    }

    #[test]
    fn frame_limiter_rejects_one_oversized_frame() {
        const CAP: usize = 4096;
        let input = vec![b'x'; CAP + 1];
        let mut reader = FrameLimitedAsyncRead {
            inner: futures::io::Cursor::new(input),
            remaining: CAP,
            cap: CAP,
        };
        let mut output = Vec::new();
        let err = futures::executor::block_on(reader.read_to_end(&mut output)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// Many small turns whose *combined* stdout is large must all succeed
    /// and keep the same session alive. Each ACP frame is bounded; neither a
    /// turn nor the persistent connection has an aggregate frame budget.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn many_small_turns_do_not_exhaust_the_per_frame_cap() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // Each turn sends ~15 KB of text, 20 turns ≈ 300 KB in all.
        let per_turn_msg = "a".repeat(15_000);
        let model = fake_model(
            &state,
            "wt:pipe-small-turns",
            vec![("BAKERY_FAKE_ACP_MESSAGE".to_string(), per_turn_msg)],
        );

        let first_pid = {
            model.chat("sys", &[ChatMsg::user("turn 0")]).unwrap();
            let sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&("wt:pipe-small-turns".to_string(), Seat::Orchestrator)) {
                Some(AcpSlot::Live(live)) => live.pid,
                _ => panic!("session must be Live after first chat"),
            }
        };

        for i in 1..20usize {
            model
                .chat("sys", &[ChatMsg::user(&format!("turn {i}"))])
                .unwrap_or_else(|e| panic!("turn {i} must succeed but got: {e}"));
        }

        let last_pid = {
            let sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&("wt:pipe-small-turns".to_string(), Seat::Orchestrator)) {
                Some(AcpSlot::Live(live)) => live.pid,
                _ => panic!("session must still be Live after 20 turns"),
            }
        };
        assert_eq!(
            first_pid, last_pid,
            "same child must survive 20 small turns"
        );
    }

    /// A child that floods stderr is killed before it can grow without bound.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_flood_of_stderr_never_ends_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = Arc::new(fake_model(
            &state,
            "wt:pipe-stderr",
            vec![("BAKERY_FAKE_ACP_FLOOD_STDERR".to_string(), "1".to_string())],
        ));

        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("sys", &[ChatMsg::user("flood")]))
        };
        let pid = wait_for_acp_pid(&state, "wt:pipe-stderr");
        assert!(running.join().unwrap().is_ok(), "however much it writes to stderr");
        let sessions = state.acp_sessions.lock().unwrap();
        let live = matches!(sessions.get(&("wt:pipe-stderr".to_string(), Seat::Orchestrator)), Some(AcpSlot::Live(l)) if l.pid == pid);
        assert!(live, "the same session goes on");
    }

    /// A reply of any length comes back whole, its session kept.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_long_reply_is_kept_whole() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = Arc::new(fake_model(
            &state,
            "wt:pipe-text",
            vec![("BAKERY_FAKE_ACP_FLOOD_TEXT".to_string(), "1".to_string())],
        ));

        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("sys", &[ChatMsg::user("flood")]))
        };
        let pid = wait_for_acp_pid(&state, "wt:pipe-text");
        let text = running.join().unwrap().unwrap();
        assert_eq!(text.len(), 300 * 1024, "every word of it");
        let sessions = state.acp_sessions.lock().unwrap();
        let live = matches!(sessions.get(&("wt:pipe-text".to_string(), Seat::Orchestrator)), Some(AcpSlot::Live(l)) if l.pid == pid);
        assert!(live, "the same session goes on");
    }

    /// 07d-A5 fix-verification review: a `session/prompt` failure that
    /// leaves the child alive (a dropped prompt future is the
    /// production case — `acp_timeout` only drops its own racing future,
    /// `io_task` and the child keep running independently) must not let a
    /// later turn on the same agent reuse that same, now-untrusted session.
    /// Reusing it could let a leftover callback from the failed turn read
    /// `GateClient.current_turn_id` after it has already been reset to the
    /// newer turn's id, vacuously satisfying `admit_child_tool_call`'s
    /// turn_id staleness guard and clobbering the newer turn's real
    /// activity phase — exactly the bug that guard exists to prevent.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_failed_session_prompt_kills_the_child_so_a_later_turn_gets_a_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = Arc::new(fake_model(
            &state,
            "wt:pipe-failprompt",
            vec![("BAKERY_FAKE_ACP_FAIL_PROMPT".to_string(), "1".to_string())],
        ));

        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("sys", &[ChatMsg::user("this call fails")]))
        };
        let pid1 = wait_for_acp_pid(&state, "wt:pipe-failprompt");
        let err = running.join().unwrap().unwrap_err();
        assert!(
            !err.is_empty(),
            "a forced session/prompt failure must fail the turn: {err}"
        );
        assert_pid_reaped(pid1);
        assert_acp_registry_clear(&state, "wt:pipe-failprompt");

        // A later turn on the same agent — a fresh model instance for the
        // same (stream_id, seat), without the fail flag — must spawn a
        // genuinely new session rather than reuse the killed one.
        let model2 = fake_model(&state, "wt:pipe-failprompt", Vec::new());
        model2
            .chat(
                "sys",
                &[ChatMsg::user("this call should get a new session")],
            )
            .unwrap();
        let pid2 = wait_for_acp_pid(&state, "wt:pipe-failprompt");
        assert_ne!(
            pid1, pid2,
            "a later turn must not reuse the killed session's pid"
        );
    }

    /// The stricter regression a second fix-verification round specifically
    /// asked for: a retry racing in immediately after a fatal prompt
    /// failure — deliberately BEFORE the killed pid has even been reaped
    /// (unlike the test above, this one never waits on `assert_pid_reaped`/
    /// `assert_acp_registry_clear` first). Proves the session is marked
    /// dead (both `closed` and the registry eviction) synchronously as part
    /// of handling the failure itself, not merely eventually once `io_task`
    /// notices the process died — closing the exact atomicity gap a
    /// `io_task.is_finished()`-only check would have left open.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_retry_racing_in_before_the_killed_pid_has_reaped_still_gets_a_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = Arc::new(fake_model(
            &state,
            "wt:pipe-failrace",
            vec![("BAKERY_FAKE_ACP_FAIL_PROMPT".to_string(), "1".to_string())],
        ));

        let pid1 = {
            let model = model.clone();
            let running =
                std::thread::spawn(move || model.chat("sys", &[ChatMsg::user("this call fails")]));
            let pid = wait_for_acp_pid(&state, "wt:pipe-failrace");
            let err = running.join().unwrap().unwrap_err();
            assert!(
                !err.is_empty(),
                "a forced session/prompt failure must fail the turn: {err}"
            );
            pid
        };

        // Deliberately race in immediately — no reap/registry-clear wait.
        let model2 = fake_model(&state, "wt:pipe-failrace", Vec::new());
        let reply2 = model2.chat("sys", &[ChatMsg::user("should get a fresh session")]);
        assert!(
            reply2.is_ok(),
            "a racing retry must never silently merge into the dying session: {reply2:?}"
        );
        let pid2 = wait_for_acp_pid(&state, "wt:pipe-failrace");
        assert_ne!(
            pid1, pid2,
            "the racing retry must spawn a genuinely new session, not reuse the dying one"
        );
    }

    /// Reassigning a seat: the old session's closed flag is set so any clone
    /// that got past ensure_boxed_session cannot reach the old child; the old
    /// pid is reaped; no Live slot remains on that seat.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn reassign_closes_the_old_acp_session_and_reaps_its_pid() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-reassign";
        let model = fake_model(&state, stream_id, Vec::new());

        // Establish a live session.
        model.chat("sys", &[ChatMsg::user("hi")]).unwrap();

        // Clone the LiveAcpSession before reassign — simulates a thread that
        // obtained it just before the reassign arrived.
        let old_live = {
            let sessions = state.acp_sessions.lock().unwrap();
            match sessions.get(&(stream_id.to_string(), Seat::Orchestrator)) {
                Some(AcpSlot::Live(live)) => live.clone(),
                _ => panic!("session must be Live"),
            }
        };
        let old_pid = old_live.pid;
        assert!(
            !old_live.is_closed(),
            "closed must be false before reassign"
        );

        // Reassign the seat to something else (stub model).
        crate::state::assign_seat_model(&state, stream_id, Seat::Orchestrator, "stub")
            .map_err(|(_, e)| e)
            .unwrap();

        // closed flag must be set — any in-flight clone's send is now blocked.
        assert!(
            old_live.is_closed(),
            "reassign must close every retained LiveAcpSession clone"
        );

        // Old pid must be gone.
        let rc = unsafe { libc::kill(old_pid as libc::pid_t, 0) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        assert!(
            rc != 0 && errno == libc::ESRCH,
            "old pid {old_pid} must be reaped after reassign (rc={rc} errno={errno})"
        );

        // No Live slot may remain on that seat.
        let sessions = state.acp_sessions.lock().unwrap();
        assert!(
            !matches!(
                sessions.get(&(stream_id.to_string(), Seat::Orchestrator)),
                Some(AcpSlot::Live(_))
            ),
            "no Live ACP slot must survive reassign"
        );
    }

    /// 07d-A3B review proof: "kill the ACP child between turns; the
    /// replacement must answer using turn one and retain the full prior
    /// response." Turn one's Heart record (`agent.turn.started` +
    /// `agent.turn.done`) is appended exactly the way
    /// `routes::agent::agent_acp_turn` does it — this test proves
    /// `session_main`'s catch-up replay independently of that route. The
    /// child is killed and the registry drained deterministically (not
    /// racing the natural EOF-detection cleanup that would eventually do
    /// the same thing), then a second `AcpPipeModel` — a fresh process,
    /// same agent_id — is configured to echo exactly what it received.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_respawned_session_after_the_child_is_killed_replays_turn_ones_full_response() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:killbetween";
        let workspace = tempfile::tempdir().unwrap();
        let workspace_path = workspace.path().to_path_buf();

        let turn_one_reply = "turn one full answer, quite specific and not a truncated preview";
        let model1 = fake_model_in_workspace(
            &state,
            stream_id,
            workspace_path.clone(),
            vec![(
                "BAKERY_FAKE_ACP_MESSAGE".to_string(),
                turn_one_reply.to_string(),
            )],
            true,
        );
        let reply1 = model1
            .chat("", &[ChatMsg::user("first instruction")])
            .unwrap();
        assert_eq!(reply1, turn_one_reply);

        // Record turn one on Heart exactly as `agent_acp_turn` would —
        // `acp_prior_turns` reads these two families by `agent_id`.
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        crate::state::append(
            &state,
            stream_id,
            "human",
            "agent.turn.started",
            serde_json::json!({"instruction": "first instruction", "agent_id": agent_id})
                .to_string(),
            vec![],
        )
        .unwrap();
        crate::state::append(
            &state,
            stream_id,
            "agent:orchestrator",
            "agent.turn.done",
            serde_json::json!({"summary": reply1, "agent_id": agent_id, "model_id": "acp"})
                .to_string(),
            vec![],
        )
        .unwrap();

        // Kill the child between turns and wait for the registry to clear —
        // deterministic, not a race against the io_task EOF detection.
        let pid = wait_for_acp_pid(&state, stream_id);
        bakery_exec::kill_process_group(pid);
        assert_pid_reaped(pid);
        assert_acp_registry_clear(&state, stream_id);

        // Turn two: the "replacement" — a brand new AcpPipeModel spawns a
        // brand new process (the map entry is gone), configured to echo
        // exactly the prompt text it received.
        let model2 = fake_model_in_workspace(
            &state,
            stream_id,
            workspace_path,
            vec![("BAKERY_FAKE_ACP_ECHO_PROMPT".to_string(), "1".to_string())],
            true,
        );
        let reply2 = model2
            .chat("", &[ChatMsg::user("second instruction")])
            .unwrap();

        assert!(
            reply2.contains(turn_one_reply),
            "the replacement child must receive turn one's FULL response as catch-up context, not a preview: {reply2}"
        );
        assert!(
            reply2.contains("second instruction"),
            "the replacement child must still receive the new instruction alongside catch-up: {reply2}"
        );
    }

    /// The operator's report: a prompt failed on a network error, the child
    /// was killed (as every failed prompt's is), and the next session's
    /// catch-up skipped the failed turn — "its original request is no longer
    /// in the history I can see". It is replayed now, with what it wrote and
    /// why it stopped; the turn in flight is not replayed to itself.
    #[test]
    fn catchup_after_a_failed_prompt_replays_the_failed_turn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:failedcatchup";
        let agent_id = crate::node::agent_id(stream_id, Seat::Orchestrator);
        let add = |actor: &str, family: &str, payload: serde_json::Value| {
            crate::state::append(&state, stream_id, actor, family, payload.to_string(), vec![])
                .unwrap();
        };
        add("human", "agent.turn.started", serde_json::json!({"instruction": "first instruction", "agent_id": agent_id, "turn_id": "t1"}));
        add("agent:orchestrator", "agent.turn.done", serde_json::json!({"summary": "first full reply", "agent_id": agent_id, "turn_id": "t1"}));
        add("human", "agent.turn.started", serde_json::json!({"instruction": "hide the tab strip", "agent_id": agent_id, "turn_id": "t2"}));
        add("agent:orchestrator", "agent.acp.text", serde_json::json!({"text": "Checking the selectors.", "agent_id": agent_id, "turn_id": "t2"}));
        add("node", "agent.turn.failed", serde_json::json!({"reason": "API Error: Can't reach the API server (ENOTFOUND)", "agent_id": agent_id, "turn_id": "t2"}));
        add("human", "agent.turn.started", serde_json::json!({"instruction": "resume", "agent_id": agent_id, "turn_id": "t3"}));

        let catchup = acp_catchup_context(&state, stream_id, Seat::Orchestrator, "t3");
        assert!(catchup.contains("Human: first instruction\nAssistant: first full reply"), "{catchup}");
        assert!(catchup.contains("Human: hide the tab strip"), "{catchup}");
        assert!(catchup.contains("did not finish — it failed: API Error"), "{catchup}");
        assert!(catchup.contains("Checking the selectors."), "{catchup}");
        assert!(!catchup.contains("Human: resume"), "the turn in flight is not its own history: {catchup}");
    }

    #[test]
    fn stale_generation_cleanup_cannot_remove_a_newer_slot() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let key = ("wt:pipe-aba".to_string(), Seat::Orchestrator);
        let old_generation = next_acp_generation();
        let new_generation = next_acp_generation();
        state.acp_sessions.lock().unwrap().insert(
            key.clone(),
            AcpSlot::Spawning {
                generation: new_generation,
                closed: Arc::new(AtomicBool::new(false)),
            },
        );

        remove_acp_slot_generation(&state, &key, old_generation);

        let sessions = state.acp_sessions.lock().unwrap();
        assert_eq!(
            sessions.get(&key).map(AcpSlot::generation),
            Some(new_generation)
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn dropping_an_old_model_cannot_remove_its_replacement_generation() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-model-aba";
        let old_model = fake_model(&state, stream_id, Vec::new());
        old_model.chat("sys", &[ChatMsg::user("old")]).unwrap();
        let old_generation = old_model.owned_generation.lock().unwrap().unwrap();

        crate::state::assign_seat_model(&state, stream_id, Seat::Orchestrator, "stub")
            .map_err(|(_, e)| e)
            .unwrap();

        let replacement = fake_model(&state, stream_id, Vec::new());
        replacement.chat("sys", &[ChatMsg::user("new")]).unwrap();
        let replacement_generation = replacement.owned_generation.lock().unwrap().unwrap();
        assert_ne!(old_generation, replacement_generation);

        drop(old_model);

        let sessions = state.acp_sessions.lock().unwrap();
        assert_eq!(
            sessions
                .get(&(stream_id.to_string(), Seat::Orchestrator))
                .map(AcpSlot::generation),
            Some(replacement_generation),
            "a stale model Drop must not clear the replacement's slot"
        );
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn reassignment_revokes_a_stale_cached_model_arc_before_it_can_respawn() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-revoked-arc";
        let model = Arc::new(fake_model(&state, stream_id, Vec::new()));
        let cached: Arc<dyn Model> = model.clone();
        crate::state::seed_seat_model_for_test(&state, stream_id, Seat::Orchestrator, cached);
        model.chat("sys", &[ChatMsg::user("before")]).unwrap();

        crate::state::assign_seat_model(&state, stream_id, Seat::Orchestrator, "stub")
            .map_err(|(_, e)| e)
            .unwrap();

        let err = model.chat("sys", &[ChatMsg::user("after")]).unwrap_err();
        assert!(
            err.contains("revoked"),
            "stale cached Arc must fail closed: {err}"
        );
        assert!(
            state
                .acp_sessions
                .lock()
                .unwrap()
                .get(&(stream_id.to_string(), Seat::Orchestrator))
                .is_none(),
            "a revoked model must not spawn a replacement child"
        );
    }

    /// A provider-neutral handshake failure (ambiguous advertised account
    /// methods with no default) must leave no live child.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn handshake_err_reaps_child() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let model = Arc::new(fake_model(
            &state,
            "wt:pipe-handshake",
            vec![
                (
                    "BAKERY_FAKE_ACP_AMBIGUOUS_AUTH".to_string(),
                    "1".to_string(),
                ),
                (
                    "BAKERY_FAKE_ACP_INITIALIZE_DELAY_MS".to_string(),
                    "200".to_string(),
                ),
            ],
        ));

        let running = {
            let model = model.clone();
            std::thread::spawn(move || model.chat("sys", &[ChatMsg::user("hi")]))
        };
        let pid = wait_for_acp_pid(&state, "wt:pipe-handshake");
        let err = running.join().unwrap().unwrap_err();
        assert!(err.contains("Setup needed"), "{err}");
        assert_pid_reaped(pid);
        assert_acp_registry_clear(&state, "wt:pipe-handshake");
    }

    /// Machine access reaches the real sandbox: a folder the operator granted
    /// is readable from a Baker conversation's runtime, and the same file
    /// stays unreadable from any other conversation's.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_machine_access_grant_opens_only_the_bakers_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let granted = tempfile::tempdir().unwrap();
        let marker = granted.path().join("note.txt");
        std::fs::write(&marker, "granted-marker").unwrap();
        {
            let mut node = state.node.lock().unwrap();
            node.baker_stream_id = Some("wt:baker-grant".to_string());
            node.machine_access
                .grants
                .push(crate::machine_access::Grant {
                    path: granted.path().canonicalize().unwrap().display().to_string(),
                    access: crate::machine_access::Access::Read,
                    scope: crate::machine_access::Scope::Baker,
                    on_path: false,
                    preset: None,
                });
        }
        let read = |stream_id: &str| {
            fake_model(
                &state,
                stream_id,
                vec![(
                    "BAKERY_FAKE_ACP_READ_FILE".to_string(),
                    marker.display().to_string(),
                )],
            )
            .chat("sys", &[ChatMsg::user("read")])
            .unwrap()
        };
        let baker = read("wt:baker-grant");
        assert!(baker.contains("read succeeded: granted-marker"), "{baker}");
        let other = read("wt:not-baker");
        assert!(other.contains("read denied"), "{other}");
    }

    /// The settings pages that list live runtimes must answer while one is
    /// running. Machine access used to hold the node lock across a call that
    /// takes it again — one open page deadlocked the whole node. Each call
    /// runs on its own thread so a deadlock fails this test instead of
    /// hanging it.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn settings_pages_answer_while_a_baker_runtime_is_live() {
        fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(f());
            });
            rx.recv_timeout(Duration::from_secs(20))
                .unwrap_or_else(|_| panic!("{what} must not deadlock"))
        }
        fn page<F: std::future::Future + Send + 'static>(what: &str, f: F)
        where
            F::Output: Send + 'static,
        {
            within(what, move || {
                tokio::runtime::Runtime::new().unwrap().block_on(f)
            });
        }

        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().baker_stream_id = Some("wt:baker-live".to_string());
        let live = fake_model(&state, "wt:baker-live", vec![]);
        live.ensure_runtime_ready().unwrap();

        let st = state.clone();
        let view = within("machine access", move || crate::machine_access::view(&st));
        assert!(view.live.iter().any(|r| r.stream_id == "wt:baker-live"));
        page(
            "agents",
            crate::agents::agents(axum::extract::State(state.clone())),
        );
        page(
            "agent storage",
            crate::agents::storage(axum::extract::State(state.clone())),
        );
        page(
            "versions",
            crate::versions::versions(axum::extract::State(state.clone())),
        );
        drop(live);
    }

    /// On a real developer machine, the offered presets make the host's own
    /// tools run inside a Baker runtime's sandbox. Machine-dependent (it uses
    /// whatever this host has installed), so it only runs on request:
    /// `cargo test -p bakery-node presets_run_the_hosts_tools -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn presets_run_the_hosts_tools_inside_a_baker_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let home = PathBuf::from(std::env::var("HOME").unwrap());
        let mut policy = crate::machine_access::MachineAccess::default();
        for preset in crate::machine_access::presets(&home) {
            println!(
                "preset {}: {:?} {}",
                preset.id, preset.unavailable, preset.detail
            );
            policy.grants.extend(preset.grants);
            policy.env.extend(preset.env);
        }
        let policy = crate::machine_access::validate(policy, &home, &home.join(".bakery")).unwrap();
        {
            let mut node = state.node.lock().unwrap();
            node.baker_stream_id = Some("wt:baker-tools".to_string());
            node.machine_access = policy;
        }
        let workspace = tempfile::tempdir().unwrap();
        let run = |command: &str| {
            AcpPipeModel::new(
                state.clone(),
                "wt:baker-tools".to_string(),
                Seat::Orchestrator,
                workspace.path().to_path_buf(),
                AcpSpawnConfig {
                    binary: fake_acp_agent_binary(),
                    argv_suffix: vec![],
                    config_copy: None,
                    extra_env: vec![("BAKERY_FAKE_ACP_EXEC".to_string(), command.to_string())],
                    trusted: true,
                    mcp: None,
                    extra_process_exec_dirs: vec![],
                    credential_dirs: vec![],
                read_only: vec![],
                },
                "acp".to_string(),
            )
            .chat("sys", &[ChatMsg::user("run")])
            .unwrap()
        };
        let mut failed = Vec::new();
        for command in [
            "git --version",
            "cc --version",
            "cargo --version",
            "rustc --version",
            "node --version",
            "pnpm --version",
        ] {
            let out = run(command);
            println!("{command} -> {out}");
            if !out.contains("exec status=0") {
                failed.push(command);
            }
        }
        println!("PATH -> {}", run("echo $PATH"));
        assert!(failed.is_empty(), "failed inside the sandbox: {failed:?}");
    }

    /// A second request for a seat's session while it is still starting —
    /// what a restarted node sees from a reconnecting pane — waits for that
    /// one start and shares it, instead of failing with "retry shortly".
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn overlapping_requests_for_a_starting_session_share_it() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = "wt:pipe-overlap";
        let delay = vec![(
            "BAKERY_FAKE_ACP_INITIALIZE_DELAY_MS".to_string(),
            "300".to_string(),
        )];
        let first = Arc::new(fake_model(&state, stream_id, delay.clone()));
        let second = fake_model(&state, stream_id, delay);

        let starting = {
            let first = first.clone();
            std::thread::spawn(move || first.ensure_runtime_ready())
        };
        wait_for_acp_pid(&state, stream_id);
        second
            .ensure_runtime_ready()
            .expect("waits for the start in flight");
        starting
            .join()
            .unwrap()
            .expect("the start in flight succeeds");
        assert_eq!(
            *first.owned_generation.lock().unwrap(),
            *second.owned_generation.lock().unwrap(),
            "one session, shared"
        );
    }

    #[test]
    fn named_config_copy_lands_at_the_requested_relative_home_path() {
        let host_home = tempfile::tempdir().unwrap();
        let bakery_base = host_home.path().join(".bakery");
        std::fs::create_dir(&bakery_base).unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("grok-config-fixture");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("profile.txt"), "planted test profile").unwrap();
        let seat_root = tempfile::tempdir().unwrap();
        let seat_home = seat_root.path().join("seat-home");
        let plan = AcpConfigCopyPlan {
            source,
            home_relative_destination: PathBuf::from(".config/grok"),
        };

        copy_config_into_seat_home(&plan, &seat_home, host_home.path(), &bakery_base).unwrap();

        assert_eq!(
            std::fs::read_to_string(seat_home.join(".config/grok/profile.txt")).unwrap(),
            "planted test profile"
        );
    }

    #[test]
    fn host_home_and_missing_sources_are_refused_as_setup_errors() {
        let host_home = tempfile::tempdir().unwrap();
        let bakery_base = host_home.path().join(".bakery");
        std::fs::create_dir(&bakery_base).unwrap();
        let seat_root = tempfile::tempdir().unwrap();
        let seat_home = seat_root.path().join("seat-home");

        let home_plan = AcpConfigCopyPlan {
            source: host_home.path().to_path_buf(),
            home_relative_destination: PathBuf::from(".config/plug"),
        };
        let home_err =
            copy_config_into_seat_home(&home_plan, &seat_home, host_home.path(), &bakery_base)
                .unwrap_err();
        assert!(home_err.contains("Setup needed"), "{home_err}");
        assert!(home_err.contains("host HOME"), "{home_err}");

        let bakery_plan = AcpConfigCopyPlan {
            source: bakery_base.clone(),
            home_relative_destination: PathBuf::from(".config/plug"),
        };
        let bakery_err =
            copy_config_into_seat_home(&bakery_plan, &seat_home, host_home.path(), &bakery_base)
                .unwrap_err();
        assert!(bakery_err.contains("Setup needed"), "{bakery_err}");
        assert!(bakery_err.contains("~/.bakery"), "{bakery_err}");

        let missing_plan = AcpConfigCopyPlan {
            source: host_home.path().join("missing-plug-config"),
            home_relative_destination: PathBuf::from(".config/plug"),
        };
        let missing_err =
            copy_config_into_seat_home(&missing_plan, &seat_home, host_home.path(), &bakery_base)
                .unwrap_err();
        assert!(missing_err.contains("Setup needed"), "{missing_err}");
        assert!(missing_err.contains("unavailable"), "{missing_err}");

        let empty_plan = AcpConfigCopyPlan {
            source: PathBuf::new(),
            home_relative_destination: PathBuf::from(".config/plug"),
        };
        let empty_err =
            copy_config_into_seat_home(&empty_plan, &seat_home, host_home.path(), &bakery_base)
                .unwrap_err();
        assert!(empty_err.contains("non-empty absolute"), "{empty_err}");
    }

    #[test]
    fn config_destination_cannot_escape_the_seat_home() {
        let host_home = tempfile::tempdir().unwrap();
        let bakery_base = host_home.path().join(".bakery");
        std::fs::create_dir(&bakery_base).unwrap();
        let source_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("plug-config");
        std::fs::create_dir(&source).unwrap();
        let seat_root = tempfile::tempdir().unwrap();
        let plan = AcpConfigCopyPlan {
            source,
            home_relative_destination: PathBuf::from("../outside"),
        };

        let err = copy_config_into_seat_home(
            &plan,
            &seat_root.path().join("seat-home"),
            host_home.path(),
            &bakery_base,
        )
        .unwrap_err();
        assert!(err.contains("Setup needed"), "{err}");
        assert!(err.contains("without traversal"), "{err}");
        assert!(!seat_root.path().join("outside").exists());
    }

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn burn_reaps_trusted_acp_revokes_stale_model_and_respawns_untrusted() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let opened =
            crate::routes::project::open_stream(&state, &open_req(&repo, "pipe-burn-revoke"))
                .unwrap();
        let stream_id = opened.context.stream_id;
        let workspace = PathBuf::from(opened.context.worktree_path);
        let target = workspace.join("ordinary.txt");
        std::fs::write(&target, "before").unwrap();
        let child_env = vec![
            (
                "BAKERY_FAKE_ACP_TOOL_KIND".to_string(),
                "execute".to_string(),
            ),
            (
                "BAKERY_FAKE_ACP_WRITE_FILE".to_string(),
                target.to_string_lossy().into_owned(),
            ),
        ];
        let model = Arc::new(fake_model_in_workspace(
            &state,
            &stream_id,
            workspace.clone(),
            child_env.clone(),
            true,
        ));
        let uncached_stale_model = fake_model_in_workspace(
            &state,
            &stream_id,
            workspace.clone(),
            child_env.clone(),
            true,
        );
        let cached: Arc<dyn Model> = model.clone();
        crate::state::seed_seat_model_for_test(&state, &stream_id, Seat::Orchestrator, cached);

        let trusted_reply = model.chat("sys", &[ChatMsg::user("before burn")]).unwrap();
        assert!(
            trusted_reply.contains("selected"),
            "trusted tool call must be approved: {trusted_reply}"
        );
        assert!(
            trusted_reply.contains("write succeeded"),
            "trusted child must start writable: {trusted_reply}"
        );
        let old_pid = wait_for_acp_pid(&state, &stream_id);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let burn_response = runtime
            .block_on(crate::routes::lifecycle::set_lifetime(
                axum::extract::State(state.clone()),
                axum::Json(crate::routes::lifecycle::LifetimeReq {
                    stream_id: stream_id.clone(),
                    lifetime: crate::node::Lifetime::Burn,
                }),
            ))
            .unwrap();
        assert_eq!(burn_response.0.lifetime, crate::node::Lifetime::Burn);
        assert_pid_reaped(old_pid);
        assert!(
            !state
                .seats
                .lock()
                .unwrap()
                .contains_key(&(stream_id.clone(), Seat::Orchestrator)),
            "Burn must evict the cached trusted ACP model"
        );
        let stale_err = model
            .chat("sys", &[ChatMsg::user("stale after burn")])
            .unwrap_err();
        assert!(
            stale_err.contains("revoked"),
            "retained trusted Arc must fail closed: {stale_err}"
        );
        let stale_spawn_err = uncached_stale_model
            .chat("sys", &[ChatMsg::user("uncached stale after burn")])
            .unwrap_err();
        assert!(
            stale_spawn_err.contains("trusted ACP spawn refused"),
            "spawn admission must re-read Burn instead of trusting a stale config: {stale_spawn_err}"
        );

        let burn_model = fake_model_in_workspace(&state, &stream_id, workspace, child_env, false);
        let burn_reply = burn_model
            .chat("sys", &[ChatMsg::user("after burn")])
            .unwrap();
        assert!(
            burn_reply.contains("cancelled"),
            "Burn must not auto-approve Execute: {burn_reply}"
        );
        assert!(
            burn_reply.contains("write denied"),
            "Burn ACP must be workspace read-only: {burn_reply}"
        );
    }

    // ─── 07d-B Phase 2 Stage C: MCP delivery + persona injection ───────────

    fn thin_workspace() -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        Box::leak(Box::new(dir)).path().to_path_buf()
    }

    /// The Checkpoint-1-adjacent phase-gate proof the spec names explicitly:
    /// a fake ACP agent standing in for Baker's session receives real
    /// `mcp_servers` at `session/new`, spawns the shim named there,
    /// round-trips `initialize` → `tools/list` → `tools/call("task_create")`
    /// against a real (test) node HTTP server, and the resulting
    /// `task.created` fact lands for real — proving the full ACP → MCP
    /// shim → `/mcp/call` → `dispatch_baker_op` chain end to end, not just
    /// that `mcp_servers` was present on the wire.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn fake_acp_baker_session_receives_mcp_servers_and_calls_task_create_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let project_id = crate::catalog::create_project(&state, "Target Project", None)
            .unwrap()
            .0
            .project_id;

        // task_create asks the operator in every mode; below, they allow it.
        crate::agent_control::set_operating_mode(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            crate::agent_control::OperatingMode::Auto,
        )
        .unwrap();
        let addr = crate::mcp::spawn_mcp_server_for_test(state.clone());
        let token = crate::mcp::mint_mcp_token(
            &state,
            &baker_home.stream_id,
            Seat::Baker,
            crate::mcp::McpScope::BakerOps,
        );

        let call_args = serde_json::json!({
            "parent_project_id": project_id,
            "title": "via acp mcp",
            "work_order": "wo-acp-mcp",
        });
        let model = AcpPipeModel::new(
            state.clone(),
            baker_home.stream_id.clone(),
            Seat::Baker,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![
                    (
                        "BAKERY_FAKE_ACP_MCP_CALL".to_string(),
                        "task_create".to_string(),
                    ),
                    (
                        "BAKERY_FAKE_ACP_MCP_ARGS".to_string(),
                        call_args.to_string(),
                    ),
                ],
                trusted: true,
                mcp: Some(McpSpawnConfig {
                    command: bakery_node_binary(),
                    args: vec!["mcp-shim".to_string()],
                    env: vec![
                        ("BAKERY_MCP_TOKEN".to_string(), token),
                        ("BAKERY_MCP_BASE_URL".to_string(), format!("http://{addr}")),
                    ],
                }),
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );

        // Creating a task asks the operator in every mode: they allow it.
        let reply = crate::testutil::allowing(&state, &baker_home.stream_id, "task_create", || {
            model.chat("system", &[ChatMsg::user("decompose the spec")])
        })
        .unwrap();
        assert!(
            reply.contains("mcp_call_ok"),
            "the MCP round trip must succeed end to end: {reply}"
        );

        let tasks = crate::tasks::task_list(&state, &project_id);
        assert_eq!(
            tasks.len(),
            1,
            "task.create over MCP must actually mint the task"
        );
        assert_eq!(tasks[0].title, "via acp mcp");

        let project_events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&project_id)
            .unwrap();
        let created_ev = project_events
            .iter()
            .find(|e| e.family == "task.created")
            .unwrap();
        assert_eq!(
            created_ev.actor, crate::actor::agent(&baker_home.stream_id, crate::node::Seat::Baker),
            "identity comes from the token's server-side binding, not a caller-supplied argument"
        );
    }

    /// Same MCP wiring, but the token is bound to `WorkerTools` scope on a
    /// task socket rather than `BakerOps` on the permanent Baker: it can
    /// call `task_complete`, but is refused the Baker-only `task.create` —
    /// the spec's "fake ACP worker ... is refused when it attempts a
    /// Baker-only op" proof, driven through the real ACP → MCP path (not
    /// just `crate::mcp`'s own unit tests, which drive `handle_mcp_call`
    /// directly).
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn fake_acp_task_worker_session_can_complete_but_is_refused_a_baker_only_op_over_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id.clone(),
                title: "Research".into(),
                work_order: "wo-worker-mcp".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        let task_stream_id = created.task.stream_id.clone();

        let addr = crate::mcp::spawn_mcp_server_for_test(state.clone());
        let token = crate::mcp::mint_mcp_token(
            &state,
            &task_stream_id,
            Seat::Orchestrator,
            crate::mcp::McpScope::WorkerTools,
        );

        let mcp = McpSpawnConfig {
            command: bakery_node_binary(),
            args: vec!["mcp-shim".to_string()],
            env: vec![
                ("BAKERY_MCP_TOKEN".to_string(), token),
                ("BAKERY_MCP_BASE_URL".to_string(), format!("http://{addr}")),
            ],
        };

        // Refused: a worker token can never reach a Baker-only op.
        let refused_model = AcpPipeModel::new(
            state.clone(),
            task_stream_id.clone(),
            Seat::Orchestrator,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![
                    (
                        "BAKERY_FAKE_ACP_MCP_CALL".to_string(),
                        "task_retire".to_string(),
                    ),
                    ("BAKERY_FAKE_ACP_MCP_ARGS".to_string(), "{}".to_string()),
                ],
                trusted: true,
                mcp: Some(mcp.clone()),
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let refused_reply = refused_model
            .chat("system", &[ChatMsg::user("try to escalate")])
            .unwrap();
        assert!(refused_reply.contains("mcp_call_error"), "{refused_reply}");
        assert!(refused_reply.contains("not available"), "{refused_reply}");

        // Allowed: the SAME token succeeds for its own scope's tool.
        // Deliberately a DIFFERENT seat than `refused_model` above: ACP
        // sessions are cached per `(stream_id, seat)` in `AppState`
        // (`acp_sessions`), so reusing `Orchestrator` here would silently
        // hand this `chat()` call the FIRST model's already-live child
        // process (still running with the first config's `extra_env`)
        // instead of spawning a fresh one from this `AcpSpawnConfig`.
        let ok_model = AcpPipeModel::new(
            state.clone(),
            task_stream_id.clone(),
            Seat::Worker,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![
                    (
                        "BAKERY_FAKE_ACP_MCP_CALL".to_string(),
                        "task_complete".to_string(),
                    ),
                    (
                        "BAKERY_FAKE_ACP_MCP_ARGS".to_string(),
                        r#"{"summary":"done via mcp"}"#.to_string(),
                    ),
                ],
                trusted: true,
                mcp: Some(mcp),
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let ok_reply = ok_model
            .chat("system", &[ChatMsg::user("finish up")])
            .unwrap();
        assert!(ok_reply.contains("mcp_call_ok"), "{ok_reply}");
    }

    /// A bogus token (never minted by this node) is refused by `/mcp/call`
    /// itself — proven end to end through the real shim/HTTP round trip, not
    /// only `crate::mcp`'s direct unit test of the same refusal.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn fake_acp_session_with_a_bogus_mcp_token_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let addr = crate::mcp::spawn_mcp_server_for_test(state.clone());

        let model = AcpPipeModel::new(
            state.clone(),
            baker_home.stream_id.clone(),
            Seat::Baker,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![
                    (
                        "BAKERY_FAKE_ACP_MCP_CALL".to_string(),
                        "task_create".to_string(),
                    ),
                    ("BAKERY_FAKE_ACP_MCP_ARGS".to_string(), "{}".to_string()),
                ],
                trusted: true,
                mcp: Some(McpSpawnConfig {
                    command: bakery_node_binary(),
                    args: vec!["mcp-shim".to_string()],
                    env: vec![
                        (
                            "BAKERY_MCP_TOKEN".to_string(),
                            "not-a-real-token".to_string(),
                        ),
                        ("BAKERY_MCP_BASE_URL".to_string(), format!("http://{addr}")),
                    ],
                }),
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let reply = model.chat("system", &[ChatMsg::user("go")]).unwrap();
        assert!(reply.contains("mcp_call_error"), "{reply}");
        assert!(
            reply.to_ascii_lowercase().contains("unknown or expired"),
            "{reply}"
        );
    }

    /// A plain project socket's session gets NO `mcp_servers` at all — the
    /// production wiring (`state::resolve_model_id_for_seat` →
    /// `state::mcp_spawn_config_for`) only builds one for the permanent
    /// Baker stream or a task socket. Proven through `AcpPipeModel`
    /// constructed the same way `resolve_model_id_for_seat` would (`mcp:
    /// None`), reporting exactly what `session/new` carried.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_plain_project_sockets_acp_session_receives_no_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let stream_id = open_stream(&state, &open_req(&repo, "ordinary"))
            .unwrap()
            .context
            .stream_id;
        for seat in [Seat::Orchestrator, Seat::Worker] {
            let persona = acp_persona_preamble(&state, &stream_id, seat);
            assert_eq!(
                persona,
                crate::agent_loop::persona_for_agent(&state, &stream_id, seat)
            );
            assert!(persona.contains(&format!("{} agent for this project", seat.as_str())));
            assert!(persona.contains("not the Baker"));
        }

        let model = AcpPipeModel::new(
            state.clone(),
            stream_id,
            Seat::Orchestrator,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![(
                    "BAKERY_FAKE_ACP_REPORT_MCP_SERVERS".to_string(),
                    "1".to_string(),
                )],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert_eq!(
            reply, "no_mcp_servers",
            "an ordinary project socket must get an empty mcp_servers list"
        );
    }

    /// The Bakery's toolkit MCP servers reach a native runtime's session,
    /// beside the Bakery's own.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_native_session_gets_the_toolkits_mcp_servers() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let docs = crate::toolkit::SetReq {
            id: None,
            kind: crate::toolkit::Kind::Mcp,
            name: "docs".into(),
            description: "the project's docs".into(),
            project_id: None,
            enabled: None,
            text: String::new(),
            command: None,
            args: vec![],
            url: Some("http://127.0.0.1:9/mcp".into()),
            event: None,
            matcher: None,
            params: None,
        };
        crate::toolkit::set(&state, &docs).unwrap();
        let model = AcpPipeModel::new(
            state.clone(),
            "wt:toolkit-mcp".to_string(),
            Seat::Orchestrator,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![("BAKERY_FAKE_ACP_REPORT_MCP_SERVERS".to_string(), "1".to_string())],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let reply = model.chat("system", &[ChatMsg::user("hi")]).unwrap();
        assert!(reply.contains("\"name\":\"docs\"") && reply.contains("127.0.0.1:9/mcp"), "{reply}");
    }

    /// 07d-B Phase 2 Stage C item 5: the exact same `agent_loop::
    /// persona_preamble` text the HTTP tool loop prepends for the permanent
    /// Baker also reaches the FIRST prompt of an ACP session on that stream
    /// — proven by echoing the literal prompt text back.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn acp_first_prompt_on_the_permanent_baker_stream_carries_the_persona_preamble() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();

        let model = AcpPipeModel::new(
            state.clone(),
            baker_home.stream_id.clone(),
            Seat::Baker,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![("BAKERY_FAKE_ACP_ECHO_PROMPT".to_string(), "1".to_string())],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let reply = model
            .chat("system", &[ChatMsg::user("hello baker")])
            .unwrap();
        assert!(reply.contains("You are Baker"), "{reply}");
        assert!(
            reply.contains("hello baker"),
            "the original prompt text must still follow the preamble: {reply}"
        );
    }

    /// A command reaches the runtime exactly as typed, the persona and the
    /// rest of the Bakery's own context waiting for the next message.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn a_command_reaches_the_runtime_exactly_as_typed() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let model = AcpPipeModel::new(
            state.clone(),
            baker_home.stream_id.clone(),
            Seat::Baker,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![("BAKERY_FAKE_ACP_ECHO_PROMPT".to_string(), "1".to_string())],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        assert_eq!(model.chat("system", &[ChatMsg::user("/compact keep the plan")]).unwrap(), "/compact keep the plan");
        let next = model.chat("system", &[ChatMsg::user("hello baker")]).unwrap();
        assert!(next.contains("You are Baker") && next.ends_with("hello baker"), "{next}");
    }

    #[test]
    fn a_command_is_a_slash_and_a_name_never_a_path() {
        for command in ["/compact", "/context", "/mcp", "/my-skill do it", "/plugin:cmd x"] {
            assert!(is_command(command), "{command}");
        }
        for text in ["/Users/op/file.rs is broken", "compact", "/", " ", "see /compact"] {
            assert!(!is_command(text), "{text}");
        }
    }

    /// Same proof, for a task socket's worker persona.
    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "needs macOS Seatbelt")]
    fn acp_first_prompt_on_a_task_socket_carries_the_task_worker_persona_preamble() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let project_id = crate::catalog::create_project(&state, "P", None)
            .unwrap()
            .0
            .project_id;
        let created = crate::tasks::task_create(
            &state,
            crate::tasks::TaskCreateReq {
                parent_project_id: project_id,
                title: "T".into(),
                work_order: "wo-persona".into(),
                contract: Default::default(),
            },
        )
        .unwrap();
        let task_stream_id = created.task.stream_id.clone();
        let note = crate::routes::memory::MemoryWriteReq {
            stream_id: task_stream_id.clone(),
            id: Some("cache".into()),
            layer: None,
            sensitivity: None,
            text: "the build cache lives in target/".into(),
            shape: Default::default(),
        };
        crate::routes::memory::memory_write_inner(&state, &note).unwrap();

        let model = AcpPipeModel::new(
            state.clone(),
            task_stream_id,
            Seat::Orchestrator,
            thin_workspace(),
            AcpSpawnConfig {
                binary: fake_acp_agent_binary(),
                argv_suffix: vec![],
                config_copy: None,
                extra_env: vec![("BAKERY_FAKE_ACP_ECHO_PROMPT".to_string(), "1".to_string())],
                trusted: true,
                mcp: None,
                extra_process_exec_dirs: vec![],
                credential_dirs: vec![],
                read_only: vec![],
            },
            "acp".to_string(),
        );
        let reply = model
            .chat("system", &[ChatMsg::user("hello worker")])
            .unwrap();
        assert!(
            reply.contains(&format!("task worker for task {}", created.task.task_id)),
            "{reply}"
        );
        assert!(
            reply.contains("hello worker"),
            "the original prompt text must still follow the preamble: {reply}"
        );
        assert!(reply.contains("[Memory:") && reply.contains("the build cache lives in target/"), "the session opens with the memory's index: {reply}");
        let next = model.chat("system", &[ChatMsg::user("and again")]).unwrap();
        assert!(!next.contains("[Memory:"), "once per session, so it stays the same for it: {next}");
    }
}
