//! Health/version, Heart stats, pending-gates queue, node identity, the
//! WebSocket event feed, and the operator dashboard snapshot.

use crate::node::WriteMode;
use crate::routes::baker::{baker_work_list_inner, bakery_source_root_from_env};
use crate::state::{known_streams, AppState};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub(crate) async fn health() -> &'static str {
    "ok"
}

/// What the Heart actually knows about itself.
///
/// Every field here is measured, never modelled. There are no projection
/// counts and no staleness figure because there are no projections: the
/// store is one append-only `events` table. A dashboard that wants L0–L4
/// rows has to wait for layers to exist rather than be handed placeholders.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct HeartStats {
    /// Streams with at least one event.
    pub(crate) streams: u64,
    /// Total events across every stream.
    pub(crate) events: u64,
    /// Oldest and newest event timestamps in the store; `None` when empty.
    pub(crate) first_ts: Option<String>,
    pub(crate) last_ts: Option<String>,
    /// On-disk footprint: `heart.db` plus its `-wal`/`-shm` sidecars.
    /// `None` when the Heart is in-memory (tests) or the files cannot be
    /// stat'ed — never 0, which would read as "empty" rather than "unknown".
    pub(crate) db_bytes: Option<u64>,
    /// Present only when the caller asked for `?verify=1`. Verification
    /// re-reads and re-hashes every event, so it is opt-in: a stats poll
    /// must not silently become an O(all events) scan, and this endpoint
    /// must not claim "chain verified" on a check it never ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) chain: Option<ChainCheck>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct ChainCheck {
    /// True only when every stream's hash chain verified end to end.
    pub(crate) verified: bool,
    pub(crate) streams_checked: u64,
    /// Streams whose chain did not verify, with the reason. Empty when
    /// `verified` — tamper is always named, never reduced to a count.
    pub(crate) broken: Vec<BrokenChain>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct BrokenChain {
    pub(crate) stream_id: String,
    pub(crate) reason: String,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct HeartQuery {
    /// `1` / `true` opts into the full chain verification.
    #[serde(default)]
    pub(crate) verify: Option<String>,
}

pub(crate) async fn heart_stats(
    State(st): State<AppState>,
    Query(q): Query<HeartQuery>,
) -> Result<Json<HeartStats>, (StatusCode, String)> {
    let verify = matches!(q.verify.as_deref(), Some("1") | Some("true"));
    heart_stats_inner(&st, verify)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

pub(crate) fn heart_stats_inner(state: &AppState, verify: bool) -> Result<HeartStats, String> {
    // One grouped scan already gives per-stream counts and the ts range.
    let summaries = {
        let heart = state.heart.lock().unwrap();
        heart.list_streams().map_err(|e| e.to_string())?
    };

    let streams = summaries.len() as u64;
    let events = summaries.iter().map(|s| s.events).sum();
    let first_ts = summaries.iter().map(|s| s.first_ts.clone()).min();
    let last_ts = summaries.iter().map(|s| s.last_ts.clone()).max();

    let chain = if verify {
        let heart = state.heart.lock().unwrap();
        let mut broken = Vec::new();
        for s in &summaries {
            if let Err(e) = heart.verify_stream(&s.stream_id) {
                broken.push(BrokenChain {
                    stream_id: s.stream_id.clone(),
                    reason: e.to_string(),
                });
            }
        }
        Some(ChainCheck {
            verified: broken.is_empty(),
            streams_checked: streams,
            broken,
        })
    } else {
        None
    };

    Ok(HeartStats {
        streams,
        events,
        first_ts,
        last_ts,
        db_bytes: heart_db_bytes(&state.base),
        chain,
    })
}

/// Size of `heart.db` and its WAL sidecars. `None` rather than 0 when the
/// main file is absent — an in-memory Heart has no footprint to report, and
/// reporting 0 would be a claim that it is empty.
pub(crate) fn heart_db_bytes(base: &std::path::Path) -> Option<u64> {
    let main = base.join("heart.db");
    let total = std::fs::metadata(&main).ok()?.len();
    // Sidecars are best-effort: WAL content is real on-disk bytes, but a
    // missing one is normal (checkpointed), not an error.
    let wal = std::fs::metadata(base.join("heart.db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    let shm = std::fs::metadata(base.join("heart.db-shm"))
        .map(|m| m.len())
        .unwrap_or(0);
    Some(total + wal + shm)
}

/// One row of the cross-stream "waiting on you" queue.
///
/// Deliberately carries NO file bodies. The client previously built this
/// queue by fetching every stream's full history and diffing `old_content`
/// against `new_content` in the browser — N round trips, each one shipping
/// whole files just to render `+3 / −1`. The counts are computed here and
/// the bytes stay on the node; a client that wants the bytes still opens the
/// stream and reviews the proposal itself.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct Gate {
    /// Only `"write"` is produced today. Elevated and handoff gates have no
    /// data source yet (NODE_API.md §8, §9) and are never synthesized here.
    pub(crate) kind: &'static str,
    pub(crate) stream_id: String,
    /// The pending `agent.file.changed` — the hash `POST /agent/accept` wants.
    pub(crate) event_hash: String,
    pub(crate) file_path: String,
    pub(crate) project: String,
    pub(crate) branch: String,
    /// Who proposed it (`agent:{model}`), not who decides it.
    pub(crate) actor: String,
    pub(crate) ts: String,
    pub(crate) added: usize,
    pub(crate) removed: usize,
}

/// Every pending write proposal this node knows, oldest first.
pub(crate) async fn list_gates(
    State(st): State<AppState>,
) -> Result<Json<Vec<Gate>>, (StatusCode, String)> {
    gates_inner(&st)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}

/// A proposal is still a gate when its own status is `pending` and no
/// accept/reject cites it.
///
/// Mirrors `decide()`'s idempotency check exactly, including the omission of
/// `agent.edit.failed`: a decision that failed to land does NOT consume the
/// proposal, so the node will still accept it and this queue must still
/// offer it. `agent.approval.cancelled`, in contrast, terminalizes the
/// exact gate when Stop wins while the turn is awaiting a human action.
pub(crate) fn gates_inner(state: &AppState) -> Result<Vec<Gate>, String> {
    let streams = known_streams(state)?;
    let mut out: Vec<Gate> = Vec::new();

    for s in streams {
        // Full read, on purpose. `read_stream_tail` is the right tool for hot
        // memory (M1b) because that window is genuinely recent-only — a
        // pending proposal can sit arbitrarily far back in a stream, so
        // tailing here would silently drop old gates, which is precisely the
        // failure a review queue must not have.
        let events = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&s.stream_id)
            .map_err(|e| e.to_string())?;

        for ev in events.iter().filter(|e| e.family == "agent.file.changed") {
            let Ok(p) = serde_json::from_str::<serde_json::Value>(&ev.payload) else {
                continue; // a malformed payload is not a decidable gate
            };
            if p.get("status").and_then(|v| v.as_str()) != Some("pending") {
                continue;
            }
            let decided = events.iter().any(|d| {
                (d.family == "agent.edit.accepted"
                    || d.family == "agent.edit.rejected"
                    || d.family == "agent.approval.cancelled")
                    && (d.causality.iter().any(|h| h == &ev.event_hash)
                        || d.payload.contains(&ev.event_hash))
            });
            if decided {
                continue;
            }
            let (added, removed) = change_counts(
                p.get("old_content").and_then(|v| v.as_str()).unwrap_or(""),
                p.get("new_content").and_then(|v| v.as_str()).unwrap_or(""),
            );
            out.push(Gate {
                kind: "write",
                stream_id: s.stream_id.clone(),
                event_hash: ev.event_hash.clone(),
                file_path: p
                    .get("file_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
                project: if s.project.is_empty() {
                    s.name.clone()
                } else {
                    s.project.clone()
                },
                branch: s.branch.clone(),
                actor: ev.actor.clone(),
                ts: ev.ts.clone(),
                added,
                removed,
            });
        }
    }

    // Oldest first: this is a queue, not a feed.
    out.sort_by(|a, b| {
        a.ts.cmp(&b.ts)
            .then_with(|| a.event_hash.cmp(&b.event_hash))
    });
    Ok(out)
}

/// Lines added plus lines removed in a changed region, past which it counts
/// as all removed then all added. Mirrors `bakery-client/src/lib/diff.ts` so
/// the node's counts and the review pane's rendered diff never disagree.
pub(crate) const MAX_EDITS: usize = 2400;

pub(crate) fn split_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return Vec::new();
    }
    let mut v: Vec<&str> = s.split('\n').collect();
    if v.last() == Some(&"") {
        v.pop(); // trailing newline is not a line
    }
    v
}

/// `(added, removed)` for a full-file change, identical to counting `+`/`-`
/// in `diff.ts`'s output: trim the common prefix/suffix, then the shortest
/// edit script of the middle. Only its length is needed, so this keeps one
/// row of furthest-reaching paths instead of the traceback.
pub(crate) fn change_counts(old: &str, new: &str) -> (usize, usize) {
    let a = split_lines(old);
    let b = split_lines(new);

    let mut p = 0;
    while p < a.len() && p < b.len() && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < a.len() - p && s < b.len() - p && a[a.len() - 1 - s] == b[b.len() - 1 - s] {
        s += 1;
    }
    let am = &a[p..a.len() - s];
    let bm = &b[p..b.len() - s];

    // Same bail-out as the client: past the cap, treat it as delete-all then
    // insert-all.
    match edit_distance(am, bm) {
        Some(edits) => {
            let common = (am.len() + bm.len() - edits) / 2;
            (bm.len() - common, am.len() - common)
        }
        None => (bm.len(), am.len()),
    }
}

/// The fewest lines to add and remove to turn `a` into `b` (Myers' O(ND)
/// difference): the work follows how many lines differ, not how long the two
/// are. `None` when more than [`MAX_EDITS`] differ.
pub(crate) fn edit_distance(a: &[&str], b: &[&str]) -> Option<usize> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        return Some((n + m) as usize);
    }
    let max = (n + m).min(MAX_EDITS as isize);
    let at = |k: isize| (max + 1 + k) as usize;
    // reach[at(k)]: how far along `a` the furthest path on diagonal k
    // (x - y = k) has got; -1 before any has.
    let mut reach = vec![-1isize; 2 * max as usize + 3];
    reach[at(1)] = 0;
    for d in 0..=max {
        for k in (-d..=d).step_by(2) {
            // After d edits: from diagonal k+1 by adding a line of `b`, from
            // k-1 by removing a line of `a`, whichever got further.
            let (above, left) = (reach[at(k + 1)], reach[at(k - 1)]);
            let mut x = -1;
            if above >= 0 && above - k <= m {
                x = above;
            }
            if left >= 0 && left < n && left + 1 > x {
                x = left + 1;
            }
            if x < 0 {
                reach[at(k)] = -1;
                continue;
            }
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            reach[at(k)] = x;
            if x == n && y == m {
                return Some(d as usize);
            }
        }
    }
    None
}

/// Identity + policy + the model/mode shown in the context bar.
pub(crate) async fn node_info(State(st): State<AppState>) -> Json<NodeInfo> {
    Json(node_info_snapshot(&st))
}

/// Shared by `GET /node` and `POST /policy/write-mode` — both return the
/// same shape so the client can refresh from either response.
pub(crate) fn node_info_snapshot(st: &AppState) -> NodeInfo {
    let node = st.node.lock().unwrap();
    NodeInfo {
        node_id: node.node_id.clone(),
        hostname: node.hostname.clone(),
        // Same spelling the config file uses (`prefer_local`), not Debug's.
        stream_fallback: node.stream_fallback,
        model: st.model.lock().unwrap().name().to_string(),
        write_mode: node.write_mode,
        worktree_base: st.worktree_base.to_string_lossy().into_owned(),
        max_tool_steps: node.agent_max_steps.map(|n| n.get()),
        bakery_source_root: node.bakery_source_root.clone(),
    }
}

#[derive(schemars::JsonSchema, Serialize)]
pub(crate) struct NodeInfo {
    pub(crate) node_id: String,
    pub(crate) hostname: String,
    pub(crate) stream_fallback: crate::node::StreamFallback,
    pub(crate) model: String,
    pub(crate) write_mode: WriteMode,
    pub(crate) worktree_base: String,
    pub(crate) max_tool_steps: Option<usize>,
    /// The checkout of Bakery's own source this node runs from, when known:
    /// the Bakery's own repo, as opposed to the operator's projects.
    pub(crate) bakery_source_root: Option<String>,
}

/// Mint a one-time ticket for the `/events` WebSocket. Sits behind the
/// operator-auth layer like every other route, so holding a ticket is
/// always downstream of holding the operator token — the WS upgrade itself
/// cannot carry an `Authorization` header from a browser, which is the
/// entire reason this indirection exists (same design as
/// `/terminal/ticket`).
pub(crate) async fn events_ticket(State(st): State<AppState>) -> impl IntoResponse {
    let ticket = crate::state::mint_ws_ticket_string();
    crate::state::mint_events_ticket(&st, ticket.clone());
    Json(serde_json::json!({ "ticket": ticket }))
}

#[derive(serde::Deserialize)]
pub(crate) struct EventsWsQuery {
    #[serde(default)]
    ticket: String,
}

/// `/events` used to be completely unguarded — any local process (or any
/// browser page: WS upgrades bypass CORS) could watch every stream's
/// events and activity. Now: allowed browser origin (or non-browser
/// caller) + a one-time ticket minted over the authenticated POST above.
pub(crate) async fn ws_events(
    ws: WebSocketUpgrade,
    axum::extract::Query(q): axum::extract::Query<EventsWsQuery>,
    headers: axum::http::HeaderMap,
    State(st): State<AppState>,
) -> axum::response::Response {
    if !crate::state::origin_allowed(&headers) {
        return (axum::http::StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    if !crate::state::consume_events_ticket(&st, &q.ticket) {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "missing, unknown, or expired events ticket — mint one via POST /events/ticket",
        )
            .into_response();
    }
    let rx = st.tx.subscribe();
    ws.on_upgrade(move |socket| handle_ws(socket, rx, st))
        .into_response()
}

/// What one window is sent of the live activity frames: every frame of a
/// conversation it follows (one it holds open), and of any other only a
/// slim frame as an agent's phase changes, for that conversation's glyph —
/// not the streamed text of every agent writing anywhere, ten times a
/// second. A window that never says what it follows gets every frame.
#[derive(Default)]
pub(crate) struct ActivityFilter {
    follow: Option<std::collections::HashSet<String>>,
    /// The phase this window last heard for each agent.
    told: std::collections::HashMap<String, String>,
}

impl ActivityFilter {
    /// What to send of one broadcast frame: itself, a slim one, or nothing.
    pub(crate) fn pass(&mut self, frame: String) -> Option<String> {
        let Some(route) = crate::state::activity_route(&frame) else {
            return Some(frame);
        };
        let followed = self
            .follow
            .as_ref()
            .is_none_or(|follow| follow.contains(&route.stream_id));
        let changed = self.told.get(&route.agent_id) != Some(&route.phase);
        if changed {
            self.told.insert(route.agent_id.clone(), route.phase.clone());
        }
        if followed {
            Some(frame)
        } else if changed {
            Some(crate::state::slim_activity(&route))
        } else {
            None
        }
    }

    /// The window now follows `streams`. Answers the ones it newly follows:
    /// it heard only their phase until now, so it is sent their frames.
    pub(crate) fn follow(&mut self, streams: std::collections::HashSet<String>) -> Vec<String> {
        let newly = match &self.follow {
            Some(old) => streams.difference(old).cloned().collect(),
            None => Vec::new(),
        };
        self.follow = Some(streams);
        newly
    }
}

/// A page's word on the conversations it follows, `{"follow": [ids]}`.
fn following(frame: &str) -> Option<std::collections::HashSet<String>> {
    let said = serde_json::from_str::<serde_json::Value>(frame).ok()?;
    let ids = said.get("follow")?.as_array()?;
    Some(ids.iter().filter_map(|id| id.as_str().map(str::to_string)).collect())
}

/// How many of each node's app windows are visible now, by their event
/// sockets: a page says so as its socket opens and as it shows or hides;
/// a socket that closes counts no more.
static VISIBLE: std::sync::Mutex<std::collections::BTreeMap<std::path::PathBuf, usize>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

fn seen(base: &std::path::Path, visible: bool) {
    let mut all = VISIBLE.lock().unwrap_or_else(|e| e.into_inner());
    let windows = all.entry(base.to_path_buf()).or_default();
    *windows = if visible { *windows + 1 } else { windows.saturating_sub(1) };
}

#[cfg(test)]
pub(crate) fn seen_for_test(base: &std::path::Path, visible: bool) {
    assert_eq!(visibility(&format!("{{\"visible\": {visible}}}")), Some(visible));
    assert_eq!(visibility("not a word"), None);
    seen(base, visible);
}

/// An app window of this node's is visible.
pub(crate) fn app_visible(state: &AppState) -> bool {
    let all = VISIBLE.lock().unwrap_or_else(|e| e.into_inner());
    all.get(state.base.as_path()).is_some_and(|windows| *windows > 0)
}

/// A page's word on its window, `{"visible": true|false}`.
fn visibility(frame: &str) -> Option<bool> {
    serde_json::from_str::<serde_json::Value>(frame).ok()?.get("visible")?.as_bool()
}

pub(crate) async fn handle_ws(mut socket: WebSocket, mut rx: broadcast::Receiver<String>, st: AppState) {
    let base = st.base.to_path_buf();
    let mut visible = false;
    let mut activity = ActivityFilter::default();
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(txt) => {
                    let Some(txt) = activity.pass(txt) else { continue };
                    if socket.send(Message::Text(txt.into())).await.is_err() {
                        break;
                    }
                }
                // Events this socket fell behind on are gone from the
                // channel: the page is told, and reloads what it keeps.
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    let said = serde_json::json!({"kind": "events-lagged", "missed": missed}).to_string();
                    if socket.send(Message::Text(said.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Text(text))) => {
                    if let Some(now) = visibility(text.as_str()).filter(|now| *now != visible) {
                        seen(&base, now);
                        visible = now;
                    }
                    if let Some(streams) = following(text.as_str()) {
                        let newly = activity.follow(streams);
                        let frames: Vec<String> = newly
                            .iter()
                            .flat_map(|stream_id| crate::state::activity_envelopes_for(&st, stream_id))
                            .filter_map(|frame| activity.pass(frame))
                            .collect();
                        let mut gone = false;
                        for frame in frames {
                            if socket.send(Message::Text(frame.into())).await.is_err() {
                                gone = true;
                                break;
                            }
                        }
                        if gone {
                            break;
                        }
                    }
                }
                Some(Ok(_)) => {}
                Some(Err(_)) => break,
            },
        }
    }
    if visible {
        seen(&base, false);
    }
}

// ─── Operator snapshot (catalog: docs/OPERATOR.md) ───────────────────────────

/// Compact baker job row for operators / agents — no absolute worktree paths
/// (those leak home layout) and no hostname.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub(crate) struct OperatorBakerJob {
    pub(crate) branch: String,
    pub(crate) stream_id: Option<String>,
    pub(crate) brief: Option<String>,
    pub(crate) worktree_exists: bool,
}

#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct OperatorBakerSlice {
    /// False when source root unknown / not git — jobs empty, see `error`.
    pub(crate) known: bool,
    pub(crate) job_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) has_origin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) gh_available: Option<bool>,
    pub(crate) jobs: Vec<OperatorBakerJob>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
}

/// Measured factory rollup. Reuses Heart / gates / baker-work logic.
/// **No hostname, no secrets, no invented tokens/cost/email.**
#[derive(schemars::JsonSchema, Debug, Serialize)]
pub(crate) struct OperatorSnapshot {
    pub(crate) write_mode: WriteMode,
    pub(crate) model: String,
    pub(crate) max_tool_steps: Option<usize>,
    pub(crate) stream_count: usize,
    pub(crate) pending_gates: usize,
    /// Sum of per-stream event counts (same substrate as GET /heart, no verify).
    pub(crate) events: u64,
    pub(crate) baker: OperatorBakerSlice,
}

pub(crate) fn operator_snapshot_inner(
    state: &AppState,
    env_root: Option<&str>,
    detect_from: Option<&std::path::Path>,
) -> OperatorSnapshot {
    let (write_mode, model) = {
        let n = state.node.lock().unwrap();
        let m = state.model.lock().unwrap().name().to_string();
        (n.write_mode, m)
    };

    let (stream_count, events) = {
        let heart = state.heart.lock().unwrap();
        match heart.list_streams() {
            Ok(sums) => {
                let events = sums.iter().map(|s| s.events).sum();
                (sums.len(), events)
            }
            Err(_) => (0, 0),
        }
    };

    let pending_gates = gates_inner(state).map(|g| g.len()).unwrap_or(0);

    let baker = match baker_work_list_inner(state, env_root, detect_from) {
        Ok(list) => OperatorBakerSlice {
            known: true,
            job_count: list.items.len(),
            base: list.base,
            has_origin: Some(list.has_origin),
            gh_available: Some(list.gh_available),
            jobs: list
                .items
                .into_iter()
                .map(|i| OperatorBakerJob {
                    branch: i.branch,
                    stream_id: i.stream_id,
                    brief: i.brief,
                    worktree_exists: i.worktree_exists,
                })
                .collect(),
            error: list.git_error,
        },
        Err(e) => OperatorBakerSlice {
            known: false,
            job_count: 0,
            base: None,
            has_origin: None,
            gh_available: None,
            jobs: Vec::new(),
            error: Some(e),
        },
    };

    OperatorSnapshot {
        write_mode,
        model,
        max_tool_steps: state.node.lock().unwrap().agent_max_steps.map(|n| n.get()),
        stream_count,
        pending_gates,
        events,
        baker,
    }
}

pub(crate) async fn operator_snapshot(
    State(st): State<AppState>,
) -> Result<Json<OperatorSnapshot>, (StatusCode, String)> {
    let detect = std::env::current_dir().ok();
    let env_root = bakery_source_root_from_env();
    let st2 = st.clone();
    crate::actor::spawn_blocking(move || {
        operator_snapshot_inner(&st2, env_root.as_deref(), detect.as_deref())
    })
    .await
    .map(Json)
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::agent::{agent_turn_inner, decide, AgentDecisionReq, AgentTurnReq};
    use crate::routes::baker::baker_assign_inner;
    use crate::routes::project::open_stream;
    use crate::state::append;
    use crate::testutil::*;
    use std::sync::Arc;

    // ─── /events: each window's activity frames ──────────────────────────────

    fn streaming(text: &str) -> crate::state::ActivityPhase {
        crate::state::ActivityPhase::Streaming { text: text.to_string() }
    }

    #[test]
    fn a_window_hears_full_frames_only_for_what_it_follows() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let mut rx = state.tx.subscribe();
        crate::state::start_agent_activity(&state, "wt:a", "wt:a:worker", "turn_1");
        crate::state::set_agent_phase(&state, "wt:a:worker", streaming("hel"));
        crate::state::set_agent_phase(&state, "wt:a:worker", streaming("hello"));
        let mut frames = Vec::new();
        while let Ok(frame) = rx.try_recv() {
            frames.push(frame);
        }
        assert!(frames.len() >= 3, "{frames:?}");
        let route = crate::state::activity_route(&frames[1]).expect("an activity frame has a route");
        assert_eq!(
            route,
            crate::state::ActivityRoute {
                stream_id: "wt:a".into(),
                agent_id: "wt:a:worker".into(),
                phase: "streaming".into(),
            }
        );
        let whole: serde_json::Value = serde_json::from_str(&frames[2]).unwrap();
        assert_eq!(whole["kind"], "activity");
        assert_eq!(whole["activity"]["phase"]["text"], "hello");
        assert_eq!(crate::state::activity_route(r#"{"kind":"ui-layout"}"#), None);
        assert_eq!(crate::state::activity_route(r#"{"stream_id":"wt:a","family":"agent.turn.done"}"#), None);

        // A window that says nothing hears every frame, as before.
        let mut quiet = ActivityFilter::default();
        for frame in &frames {
            assert_eq!(quiet.pass(frame.clone()).as_deref(), Some(frame.as_str()));
        }

        // Following another conversation: only each phase change, slim.
        let mut other = ActivityFilter::default();
        assert!(other.follow(["wt:b".to_string()].into()).is_empty());
        let heard: Vec<String> = frames.iter().filter_map(|f| other.pass(f.clone())).collect();
        assert_eq!(heard.len(), 2, "contacting the model, then streaming: {heard:?}");
        let slim: serde_json::Value = serde_json::from_str(&heard[1]).unwrap();
        assert_eq!(slim["slim"], true);
        assert_eq!(slim["activity"]["phase"]["phase"], "streaming");
        assert_eq!(slim["activity"]["stream_id"], "wt:a");
        assert!(slim["activity"]["phase"].get("text").is_none(), "no streamed text");
        let event = r#"{"stream_id":"wt:a","family":"agent.turn.done"}"#.to_string();
        assert_eq!(other.pass(event.clone()), Some(event), "Heart events pass untouched");

        // Following it: whole frames, and the newly followed one's now.
        assert_eq!(other.follow(["wt:a".to_string(), "wt:b".to_string()].into()), vec!["wt:a".to_string()]);
        let now = crate::state::activity_envelopes_for(&state, "wt:a");
        assert_eq!(now.len(), 1);
        assert_eq!(other.pass(now[0].clone()).as_deref(), Some(now[0].as_str()));
        assert_eq!(other.pass(frames[2].clone()).as_deref(), Some(frames[2].as_str()));
        assert!(crate::state::activity_envelopes_for(&state, "wt:b").is_empty());
    }

    #[test]
    fn a_page_says_what_it_follows() {
        assert_eq!(following(r#"{"follow":["wt:a","wt:b"]}"#), Some(["wt:a".to_string(), "wt:b".to_string()].into()));
        assert_eq!(following(r#"{"follow":[]}"#), Some(Default::default()));
        assert_eq!(following(r#"{"visible":true}"#), None);
        assert_eq!(following("not a word"), None);
    }

    // ─── GET /heart: measured store stats ────────────────────────────────────

    #[test]
    fn heart_stats_are_zero_on_an_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));

        let s = heart_stats_inner(&state, false).unwrap();
        assert_eq!(s.streams, 0);
        assert_eq!(s.events, 0);
        assert!(s.first_ts.is_none() && s.last_ts.is_none(), "{s:?}");
    }

    #[test]
    fn heart_stats_count_streams_and_events() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "one")).unwrap();
        open_stream(&state, &open_req(&repo, "two")).unwrap();

        let s = heart_stats_inner(&state, false).unwrap();
        assert_eq!(s.streams, 2, "{s:?}");

        // Cross-check against the store rather than hardcoding the per-open
        // event count — that would break every time onboarding gains a step.
        let expected: u64 = state
            .heart
            .lock()
            .unwrap()
            .list_streams()
            .unwrap()
            .iter()
            .map(|x| x.events)
            .sum();
        assert_eq!(s.events, expected, "{s:?}");
        assert!(s.events > 0);
        assert!(s.first_ts.is_some() && s.last_ts.is_some(), "{s:?}");
        assert!(
            s.first_ts.as_deref().unwrap() <= s.last_ts.as_deref().unwrap(),
            "{s:?}"
        );
    }

    #[test]
    fn heart_stats_omit_the_chain_check_unless_asked() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "quiet")).unwrap();

        let s = heart_stats_inner(&state, false).unwrap();
        assert!(
            s.chain.is_none(),
            "must not claim a verification it never ran: {s:?}"
        );

        // The field is absent from the wire too, not merely null.
        let wire = serde_json::to_string(&s).unwrap();
        assert!(!wire.contains("chain"), "{wire}");
        assert!(!wire.contains("verified"), "{wire}");
    }

    #[test]
    fn heart_stats_verify_reports_an_intact_chain() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "one")).unwrap();
        open_stream(&state, &open_req(&repo, "two")).unwrap();

        let s = heart_stats_inner(&state, true).unwrap();
        let chain = s.chain.expect("verify=1 must produce a chain result");
        assert!(chain.verified, "{chain:?}");
        assert_eq!(chain.streams_checked, 2);
        assert!(chain.broken.is_empty(), "{chain:?}");
    }

    #[test]
    fn heart_db_bytes_is_none_for_an_in_memory_heart() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        // Tests run on an in-memory Heart — there is no file, and "unknown"
        // must not be reported as 0 (which would read as "empty").
        assert!(heart_stats_inner(&state, false).unwrap().db_bytes.is_none());
    }

    #[test]
    fn heart_db_bytes_sums_the_db_and_its_wal_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        assert!(heart_db_bytes(dir.path()).is_none(), "no db file yet");

        std::fs::write(dir.path().join("heart.db"), vec![0u8; 100]).unwrap();
        assert_eq!(heart_db_bytes(dir.path()), Some(100), "main file alone");

        std::fs::write(dir.path().join("heart.db-wal"), vec![0u8; 25]).unwrap();
        assert_eq!(
            heart_db_bytes(dir.path()),
            Some(125),
            "WAL bytes are real on-disk bytes"
        );
    }

    // ─── GET /gates: cross-stream pending write proposals ────────────────────

    #[test]
    fn gates_are_empty_with_no_streams() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(gates_inner(&state).unwrap().is_empty());
    }

    #[test]
    fn gates_are_empty_when_a_stream_has_no_proposals() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        open_stream(&state, &open_req(&repo, "quiet")).unwrap();
        assert!(gates_inner(&state).unwrap().is_empty());
    }

    #[test]
    fn gates_list_one_pending_proposal_with_counts_and_no_bodies() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();

        let gates = gates_inner(&state).unwrap();
        assert_eq!(gates.len(), 1, "{gates:?}");
        let g = &gates[0];
        assert_eq!(g.kind, "write");
        assert_eq!(g.stream_id, stream_id);
        assert_eq!(
            g.event_hash, hash,
            "the hash must be the one /agent/accept wants"
        );
        assert_eq!(g.file_path, "f.txt");
        assert_eq!(g.branch, "wtA");
        assert_eq!(g.actor, "agent:orchestrator");
        assert!(!g.ts.is_empty());
        // StubModel prepends one marker line to "line one\n" and keeps it.
        assert_eq!((g.added, g.removed), (1, 0), "{g:?}");

        // The whole point of the endpoint: counts travel, bytes do not.
        let wire = serde_json::to_string(&gates).unwrap();
        assert!(!wire.contains("old_content"), "{wire}");
        assert!(!wire.contains("new_content"), "{wire}");
        assert!(
            !wire.contains("line one"),
            "file bodies must not reach the wire: {wire}"
        );
    }

    #[test]
    fn gates_span_multiple_streams_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let model = Arc::new(crate::testutil::ScriptedModel::new(&[
            r#"{"edit":{"file_path":"f.txt","new_content":"first change\n","rationale":"first"}}"#,
            r#"{"edit":{"file_path":"f.txt","new_content":"second change\n","rationale":"second"}}"#,
        ]));
        let state = state_with(dir.path(), model);
        let id_one = open_stream(&state, &open_req(&repo, "one"))
            .unwrap()
            .context
            .stream_id;
        let id_two = open_stream(&state, &open_req(&repo, "two"))
            .unwrap()
            .context
            .stream_id;

        // Manual proposals park their turns (continue-after-decision) —
        // issue each on a thread and collect the recorded proposals.
        let first_hash =
            crate::testutil::spawn_turn_and_await_pending(&state, &id_one, Some("f.txt"), "first");
        let second_hash =
            crate::testutil::spawn_turn_and_await_pending(&state, &id_two, Some("f.txt"), "second");
        let first = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_one)
            .unwrap()
            .into_iter()
            .find(|e| e.event_hash == first_hash)
            .unwrap();
        let second = state
            .heart
            .lock()
            .unwrap()
            .read_stream(&id_two)
            .unwrap()
            .into_iter()
            .find(|e| e.event_hash == second_hash)
            .unwrap();

        let gates = gates_inner(&state).unwrap();
        assert_eq!(gates.len(), 2, "one queue across every stream: {gates:?}");
        let ids: Vec<&str> = gates.iter().map(|g| g.stream_id.as_str()).collect();
        assert!(
            ids.contains(&id_one.as_str()) && ids.contains(&id_two.as_str()),
            "{ids:?}"
        );

        // Oldest first — a queue, not a feed. Timestamps can tie at second
        // resolution, so assert the ordering key rather than a strict index.
        assert!(gates[0].ts <= gates[1].ts, "{gates:?}");
        let hashes: Vec<&str> = gates.iter().map(|g| g.event_hash.as_str()).collect();
        assert!(hashes.contains(&first.event_hash.as_str()));
        assert!(hashes.contains(&second.event_hash.as_str()));
    }

    #[test]
    fn gates_omit_an_accepted_proposal() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        assert_eq!(gates_inner(&state).unwrap().len(), 1);

        decide(
            &state,
            &AgentDecisionReq {
                stream_id,
                event_hash: hash,
            },
            true,
        )
        .unwrap();

        assert!(
            gates_inner(&state).unwrap().is_empty(),
            "an accepted proposal is no longer waiting on anyone"
        );
    }

    #[test]
    fn gates_omit_a_rejected_proposal() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        decide(
            &state,
            &AgentDecisionReq {
                stream_id,
                event_hash: hash,
            },
            false,
        )
        .unwrap();
        assert!(gates_inner(&state).unwrap().is_empty());
    }

    #[test]
    fn gates_omit_auto_accepted_proposals() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with_write_mode(dir.path(), Arc::new(StubModel), WriteMode::Auto);
        let stream_id = open_stream(&state, &open_req(&repo, "autow"))
            .unwrap()
            .context
            .stream_id;

        agent_turn_inner(
            &state,
            &AgentTurnReq {
                model_id: None,
                effort: None,
                actor: None,
                stream_id,
                file_path: Some("f.txt".into()),
                instruction: "add marker".into(),
                seat: None,
            },
        )
        .unwrap();

        assert!(
            gates_inner(&state).unwrap().is_empty(),
            "policy/auto already decided it — nothing is waiting on a human"
        );
    }

    /// `decide()` refuses only on accept/reject, so a decision that FAILED
    /// leaves the proposal acceptable. The queue must agree, or a pending
    /// proposal becomes unreachable after one failed write.
    #[test]
    fn gates_still_list_a_proposal_whose_decision_failed() {
        let (_dir, state, _wt, hash, stream_id) = setup_with_pending();
        append(
            &state,
            &stream_id,
            "node",
            "agent.edit.failed",
            serde_json::json!({
                "decided_event": hash,
                "file_path": "f.txt",
                "decision": "accept",
                "reason": "disk full",
            })
            .to_string(),
            vec![hash.clone()],
        )
        .unwrap();

        let gates = gates_inner(&state).unwrap();
        assert_eq!(
            gates.len(),
            1,
            "a failed decision does not consume the proposal"
        );
        assert_eq!(gates[0].event_hash, hash);

        // And the node genuinely still accepts it — the queue is not lying.
        decide(
            &state,
            &AgentDecisionReq {
                stream_id,
                event_hash: hash,
            },
            true,
        )
        .expect("a failed decision must not block a retry");
    }

    #[test]
    fn change_counts_match_the_clients_diff_semantics() {
        // Pure insertion / deletion.
        assert_eq!(change_counts("a\nb\n", "a\nx\nb\n"), (1, 0));
        assert_eq!(change_counts("a\nx\nb\n", "a\nb\n"), (0, 1));
        // Replacement is one of each.
        assert_eq!(change_counts("a\nb\n", "a\nc\n"), (1, 1));
        // Identical content, including the empty file, is no change at all.
        assert_eq!(change_counts("a\nb\n", "a\nb\n"), (0, 0));
        assert_eq!(change_counts("", ""), (0, 0));
        // A new file is all additions; a cleared file is all deletions.
        assert_eq!(change_counts("", "a\nb\n"), (2, 0));
        assert_eq!(change_counts("a\nb\n", ""), (0, 2));
        // Trailing newline is not a line, so these are equal.
        assert_eq!(change_counts("a", "a\n"), (0, 0));
        // Past the pairing cap it degrades to delete-all + insert-all.
        let big_a = "x\n".repeat(MAX_EDITS / 2 + 5);
        let big_b = "y\n".repeat(MAX_EDITS / 2 + 3);
        assert_eq!(change_counts(&big_a, &big_b), (MAX_EDITS / 2 + 3, MAX_EDITS / 2 + 5));
    }

    /// A few edits far apart in a long file are a few lines, not the file
    /// between them: a 14-line merge into a 2683-line file counted as
    /// +1904 −1890 when its first and last edits were more than 1200 lines
    /// apart.
    #[test]
    fn edits_far_apart_in_a_long_file_count_as_themselves() {
        let file: Vec<String> = (0..2683).map(|i| format!("line {i}")).collect();
        let text = |lines: &[String]| lines.iter().map(|l| format!("{l}\n")).collect::<String>();
        // The merge's four hunks (1 + 2 + 5 + 6 lines), last first so each
        // place is the old file's.
        let mut merged = file.clone();
        for (at, count) in [(1998, 6), (1972, 5), (1955, 2), (108, 1)] {
            let made = (0..count).map(|i| format!("merged {at}.{i}"));
            merged.splice(at..at, made);
        }
        assert_eq!(change_counts(&text(&file), &text(&merged)), (14, 0));
        let mut ends = file.clone();
        ends[0] = "first, edited".into();
        ends[2682] = "last, edited".into();
        assert_eq!(change_counts(&text(&file), &text(&ends)), (2, 2));
        let mut far = file.clone();
        far.insert(100, "after the top".into());
        far.remove(2600);
        assert_eq!(change_counts(&text(&file), &text(&far)), (1, 1));
    }

    /// The counts are the shortest script's, whatever the lines: against a
    /// plain table of common subsequences.
    #[test]
    fn change_counts_are_the_shortest_scripts_over_many_pairs() {
        fn lcs(a: &[&str], b: &[&str]) -> usize {
            let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
            for i in (0..a.len()).rev() {
                for j in (0..b.len()).rev() {
                    t[i][j] = if a[i] == b[j] { t[i + 1][j + 1] + 1 } else { t[i + 1][j].max(t[i][j + 1]) };
                }
            }
            t[0][0]
        }
        let mut seed = 7u64;
        let mut rnd = |n: usize| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((seed >> 33) as usize) % n
        };
        let words = ["a", "b", "c", "d", "e", "f"];
        for round in 0..1500 {
            let alphabet = 1 + rnd(6);
            let a: Vec<&str> = (0..rnd(40)).map(|_| words[rnd(alphabet)]).collect();
            let mut b = a.clone();
            for _ in 0..rnd(12) {
                let at = rnd(b.len() + 1);
                match rnd(3) {
                    0 => b.insert(at, words[rnd(alphabet + 2) % 6]),
                    1 if !b.is_empty() => {
                        b.remove(at % b.len());
                    }
                    _ if !b.is_empty() => {
                        let line = at % b.len();
                        b[line] = words[rnd(alphabet + 2) % 6];
                    }
                    _ => {}
                }
            }
            if round % 7 == 0 {
                b = (0..rnd(40)).map(|_| words[rnd(alphabet)]).collect();
            }
            let text = |lines: &[&str]| lines.iter().map(|l| format!("{l}\n")).collect::<String>();
            let kept = lcs(&a, &b);
            assert_eq!(change_counts(&text(&a), &text(&b)), (b.len() - kept, a.len() - kept), "{a:?} -> {b:?}");
        }
    }

    // ─── Operator snapshot ───────────────────────────────────────────────────

    #[test]
    fn operator_snapshot_measured_fields_no_hostname_or_spend() {
        let dir = tempfile::tempdir().unwrap();
        let repo = bakery_repo_for_test(dir.path());
        let state = state_with_bakery_root(dir.path(), &repo);
        baker_assign_inner(&state, "snap job", None, None).unwrap();

        let snap = operator_snapshot_inner(&state, None, None);
        assert_eq!(snap.write_mode, WriteMode::Manual);
        assert!(!snap.model.is_empty());
        assert!(snap.stream_count >= 1);
        assert_eq!(snap.pending_gates, 0);
        assert!(snap.events > 0);
        assert!(snap.baker.known);
        assert_eq!(snap.baker.job_count, 1);
        assert_eq!(snap.baker.jobs[0].branch, "baker/snap-job");
        assert!(snap.baker.jobs[0].stream_id.is_some());

        let wire = serde_json::to_string(&snap).unwrap();
        let lower = wire.to_ascii_lowercase();
        assert!(!lower.contains("hostname"), "{wire}");
        assert!(!lower.contains("email"), "{wire}");
        assert!(!lower.contains("spend"), "{wire}");
        assert!(!lower.contains("cost_usd"), "{wire}");
        assert!(!lower.contains("token"), "{wire}");
        // Absolute home paths must not appear (no worktree_path field).
        assert!(!wire.contains("worktree_path"), "{wire}");
        // Value-based: full node_id (hostname-slug + hex) must not appear even
        // under another key name (same pattern as format_baker_ops leak test).
        let node_id = state.node.lock().unwrap().node_id.clone();
        assert!(
            !lower.contains(&node_id.to_ascii_lowercase()),
            "node_id must not appear on operator snapshot wire: {node_id}"
        );
        if let Some(slug) = node_id.rsplit_once('-').map(|(s, _)| s) {
            if slug.len() >= 4 {
                assert!(
                    !lower.contains(&slug.to_ascii_lowercase()),
                    "hostname slug {slug:?} must not appear on wire: {wire}"
                );
            }
        }
    }

    #[test]
    fn operator_snapshot_unknown_baker_root_is_honest() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        state.node.lock().unwrap().bakery_source_root = None;

        // env_root / detect_from hardcoded None — process env cannot hijack
        // (resolve does not read BAKERY_SOURCE_ROOT; no skip guard).
        let snap = operator_snapshot_inner(&state, None, None);
        assert!(!snap.baker.known);
        assert_eq!(snap.baker.job_count, 0);
        assert!(snap.baker.jobs.is_empty());
        assert!(snap.baker.error.is_some());
    }
}
