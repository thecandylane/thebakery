//! Seat assignment: which model id answers turns on a stream's `orchestrator`,
//! `worker`, or `baker` seat (STEERING.md § Models: "seats ≠ models").
//!
//! Assignment itself never checks live credentials — `/agent/turn` is where
//! a seat with no reachable model fails closed (06-SEATS.md §2). This route
//! only records the declared fact and invalidates any stale cached
//! resolution for it (`state::assign_seat_model`).
//!
//! The retiring/discarded check lives entirely inside `assign_seat_model`,
//! under the lifecycle lock alongside the Heart append (06-SEATS audit item
//! 6) — this route does not also call the read-only
//! `refuse_if_retiring_or_discarded` first. That would just reintroduce the
//! same "check, then race the real check-and-act" gap the lock-held version
//! exists to close.

use crate::node::Seat;
use crate::state::{assign_seat_model_or_wait, context_for, AppState, Landing, Switched};
use axum::{extract::State, http::StatusCode, Json};
use bakery_core::Event;
use serde::Deserialize;

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SeatAssignReq {
    pub(crate) stream_id: String,
    pub(crate) seat: Seat,
    pub(crate) model_id: String,
    /// Claude native's own setting — "subscription" | "console" |
    /// "openrouter"; refused on any other model, and on an unknown id.
    /// Left out, a native seat keeps whichever it already had (default
    /// subscription).
    #[serde(default)]
    pub(crate) provider: Option<String>,
    /// Why, for a switch you decide: recorded with the model the seat had.
    #[serde(default)]
    pub(crate) reason: Option<String>,
    /// The hash of the event that caused the switch. A model.quota_exhausted
    /// hands the conversation over to the new model when its last turn
    /// stopped on that quota.
    #[serde(default)]
    pub(crate) because: Option<String>,
}

/// `POST /seat/assign`: while the seat's turn runs, the switch waits for
/// that turn to end and the turn runs on; the answer is then its
/// `seat.model.pending` (`state::assign_seat_model_or_wait`).
pub(crate) async fn assign_seat(
    State(st): State<AppState>,
    Json(req): Json<SeatAssignReq>,
) -> Result<Json<Event>, (StatusCode, String)> {
    fn given(v: &Option<String>) -> Option<&str> {
        v.as_deref().map(str::trim).filter(|v| !v.is_empty())
    }
    let (reason, because) = (given(&req.reason), given(&req.because));
    // A switch with a cause is a decision, recorded as one either way.
    let reason = reason.or(because.map(|_| "no reason given"));
    let switch = Switch { model_id: &req.model_id, provider: req.provider.as_deref(), reason, because };
    seat_switch(&st, &req.stream_id, req.seat, &switch, Landing::AfterTurn).map(|switched| Json(switched.into_event()))
}

/// What a seat switches to, and why.
#[derive(Clone, Copy)]
struct Switch<'a> {
    model_id: &'a str,
    /// Claude native only: which provider.
    provider: Option<&'a str>,
    reason: Option<&'a str>,
    /// The event that caused it.
    because: Option<&'a str>,
}

/// A seat's switch, as `POST /seat/assign` makes it: checked, then landed
/// (as `landing` says), and when it lands because of a quota, the
/// conversation handed over to its new model.
fn seat_switch(state: &AppState, stream_id: &str, seat: Seat, switch: &Switch, landing: Landing) -> Result<Switched, (StatusCode, String)> {
    let switched = seat_assign_for(state, stream_id, seat, switch, landing)?;
    if let (Switched::Now(event), Some(because), Some(reason)) = (&switched, switch.because, switch.reason) {
        let from = serde_json::from_str::<crate::node::SeatAssignment>(&event.payload)
            .ok()
            .and_then(|a| a.from);
        let to = switch.model_id.trim();
        crate::model_proxy::hand_over(state, stream_id, seat, because, (from.as_deref(), to, reason));
    }
    Ok(switched)
}

/// The switch waiting for `seat`'s turn to end, landed by that turn's
/// lease: at its end, or at the next turn's start when the node restarted
/// under it. One replaced or cancelled after it was read here doesn't land.
/// One that can't land on its `last` try is recorded as failed
/// (`seat.model.switch_failed`, naming it): it waits no more, and the seat's
/// picker says why.
pub(crate) fn switch_waiting(state: &AppState, stream_id: &str, seat: Seat, last: bool) {
    let waiting = crate::state::seat_switch_waiting_locked(&state.heart.lock().unwrap(), stream_id, seat);
    let Some((event, waiting)) = waiting else {
        return;
    };
    let switch = Switch {
        model_id: &waiting.model_id,
        provider: waiting.provider.as_deref(),
        reason: waiting.reason.as_deref(),
        because: event.causality.first().map(String::as_str),
    };
    let Err((code, why)) = seat_switch(state, stream_id, seat, &switch, Landing::Waiting(&event.event_hash)) else {
        return;
    };
    eprintln!("bakery-node: {stream_id}'s {} seat, switching to {}: {why}", seat.as_str(), waiting.model_id);
    if last && code != StatusCode::CONFLICT {
        let failed = crate::node::SeatSwitchFailed { seat, error: why };
        if let Ok(payload) = serde_json::to_string(&failed) {
            let _ = crate::state::append(state, stream_id, "node", crate::node::SEAT_SWITCH_FAILED_FAMILY, payload, vec![event.event_hash]);
        }
    }
}

/// `assign_seat`'s real body — the factory/project seat-boundary check plus
/// the actual assignment. Pulled out so `crate::baker_ops`'s `seat.assign`
/// op calls the exact same checked path a human's `POST /seat/assign`
/// does, rather than re-deriving the boundary rule a second time (07d-B
/// Stage B item 1: "never a second implementation").
pub(crate) fn seat_assign_checked(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    model_id: &str,
) -> Result<Event, (StatusCode, String)> {
    let switch = Switch { model_id, provider: None, reason: None, because: None };
    seat_assign_for(state, stream_id, seat, &switch, Landing::Now).map(Switched::into_event)
}

/// [`seat_assign_checked`] as a decision: its reason and cause recorded,
/// and (Claude native only) which provider it switches to.
fn seat_assign_for(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    switch: &Switch,
    landing: Landing,
) -> Result<Switched, (StatusCode, String)> {
    let ctx = context_for(state, stream_id).map_err(|e| (StatusCode::NOT_FOUND, e))?;

    // 07d-A1 §4: Baker is factory-only — a project socket may never be
    // assigned the baker seat, and the factory socket may never be assigned
    // an ordinary project seat.
    let is_factory = crate::routes::agent::is_baker_stream_ctx(state, &ctx);
    if is_factory != (seat == Seat::Baker) {
        let msg = if is_factory {
            format!(
                "the {} seat cannot be assigned on the factory socket — only baker can",
                seat.as_str()
            )
        } else {
            "the baker seat cannot be assigned on a project socket".to_string()
        };
        return Err((StatusCode::BAD_REQUEST, msg));
    }

    let Switch { model_id, provider, reason, because } = *switch;
    assign_seat_model_or_wait(state, stream_id, seat, model_id, reason, because, provider, landing)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Lifetime, SEAT_ASSIGNED_FAMILY};
    use crate::routes::project::open_stream;
    use crate::state::{begin_discard, resolve_seat, set_non_burn_lifetime};
    use crate::testutil::*;
    use bakery_agent::{secrets, ClaudeModel, StubModel};
    use std::sync::Arc;

    /// `tempfile::tempdir()`'s own path, handed to
    /// `secrets::test_support::with_isolated_credentials` so these tests
    /// prove the seat/Heart plumbing deterministically instead of branching
    /// on whatever this developer machine happens to have configured
    /// (06b-PIPE.md: "Do not: Live secrets.json in tests").
    fn isolated_secrets_path() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        (dir, path)
    }

    fn req(stream_id: &str, seat: Seat, model_id: &str) -> SeatAssignReq {
        SeatAssignReq {
            stream_id: stream_id.to_string(),
            seat,
            model_id: model_id.to_string(),
            provider: None,
            reason: None,
            because: None,
        }
    }

    #[tokio::test]
    async fn assign_seat_refuses_an_unknown_stream() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let err = assign_seat(
            State(state),
            Json(req("wt:nope", Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn assign_seat_refuses_an_empty_model_id() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatempty = open_stream(&state, &open_req(&repo, "seatempty"))
            .unwrap()
            .context
            .stream_id;

        let err = assign_seat(
            State(state),
            Json(req(&id_seatempty, Seat::Orchestrator, "   ")),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn assign_seat_persists_a_heart_event_readable_as_the_latest_assignment() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatok = open_stream(&state, &open_req(&repo, "seatok"))
            .unwrap()
            .context
            .stream_id;

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatok, Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap();
        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatok, Seat::Worker, "claude")),
        )
        .await
        .unwrap();

        let events = state.heart.lock().unwrap().read_stream(&id_seatok).unwrap();
        let assigned: Vec<_> = events
            .iter()
            .filter(|e| e.family == SEAT_ASSIGNED_FAMILY)
            .collect();
        assert_eq!(
            assigned.len(),
            2,
            "one event per assignment, both seats on the same stream"
        );
    }

    /// `bakery_agent::resolve_seat_model`'s `#[cfg(test)]` "stub" arm only
    /// compiles into *bakery-agent's own* test binary — as a regular
    /// dependency of bakery-node, it never sees that cfg, so this proves the
    /// full assign → Heart → resolve path with a real provider id instead:
    /// whatever `resolve_seat` returns (or fails with) must reflect THIS
    /// seat's own assigned id, not a stale or wrong one. Isolated credentials
    /// make both directions of this deterministic rather than branching on
    /// ambient machine state.
    #[tokio::test]
    async fn a_turn_resolves_the_model_id_assigned_to_its_seat_from_the_heart_without_credentials()
    {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatturn = open_stream(&state, &open_req(&repo, "seatturn"))
            .unwrap()
            .context
            .stream_id;

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatturn, Seat::Worker, "claude")),
        )
        .await
        .unwrap();

        let (_guard, secrets_path) = isolated_secrets_path();
        secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let err = resolve_seat(&state, &id_seatturn, Seat::Worker)
                .map(|_| ())
                .unwrap_err();
            assert!(err.contains("claude"), "{err}");
        });
    }

    #[tokio::test]
    async fn a_turn_resolves_the_model_id_assigned_to_its_seat_from_the_heart_with_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatturncred = open_stream(&state, &open_req(&repo, "seatturncred"))
            .unwrap()
            .context
            .stream_id;

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatturncred, Seat::Worker, "claude")),
        )
        .await
        .unwrap();

        let (_guard, secrets_path) = isolated_secrets_path();
        secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let mut s = secrets::SecretsFile::default();
            s.anthropic_api_key = Some("test-only-api-key-not-a-real-credential".into());
            secrets::save_to(&secrets_path, &s).unwrap();
            assert!(ClaudeModel::credentials_available());
            assert_eq!(
                resolve_seat(&state, &id_seatturncred, Seat::Worker)
                    .unwrap()
                    .name(),
                "claude"
            );
        });
    }

    /// 06-SEATS audit item 5: a malformed *newer* assignment must fail
    /// closed, not defer to an older good one for the same seat — the
    /// forged event here isn't valid JSON at all (never something
    /// `assign_seat_model` itself would write), simulating a record this
    /// build genuinely can't read.
    #[tokio::test]
    async fn a_malformed_newer_seat_assignment_fails_closed_not_an_older_good_one() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatbad = open_stream(&state, &open_req(&repo, "seatbad"))
            .unwrap()
            .context
            .stream_id;

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatbad, Seat::Orchestrator, "claude")),
        )
        .await
        .unwrap();
        crate::state::append(
            &state,
            &id_seatbad,
            "human",
            SEAT_ASSIGNED_FAMILY,
            "not valid json at all".to_string(),
            vec![],
        )
        .unwrap();

        let err = resolve_seat(&state, &id_seatbad, Seat::Orchestrator)
            .map(|_| ())
            .unwrap_err();
        assert!(err.contains("unreadable"), "{err}");
        assert!(
            !err.to_ascii_lowercase().contains("claude"),
            "must not silently fall back to the older good assignment: {err}"
        );
    }

    /// The mirror case: an *older* malformed record must not block
    /// resolution once a newer, valid same-seat record already answers —
    /// the scan returns the instant it finds the newest match, so an older
    /// record (malformed or not) is never even reached. Isolated
    /// credentials make both directions deterministic.
    #[tokio::test]
    async fn an_older_malformed_record_does_not_block_a_newer_good_assignment_without_credentials()
    {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatoldbad = open_stream(&state, &open_req(&repo, "seatoldbad"))
            .unwrap()
            .context
            .stream_id;

        crate::state::append(
            &state,
            &id_seatoldbad,
            "human",
            SEAT_ASSIGNED_FAMILY,
            "not valid json at all".to_string(),
            vec![],
        )
        .unwrap();
        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatoldbad, Seat::Orchestrator, "claude")),
        )
        .await
        .unwrap();

        let (_guard, secrets_path) = isolated_secrets_path();
        secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let err = resolve_seat(&state, &id_seatoldbad, Seat::Orchestrator)
                .map(|_| ())
                .unwrap_err();
            assert!(err.contains("claude"), "{err}");
        });
    }

    #[tokio::test]
    async fn an_older_malformed_record_does_not_block_a_newer_good_assignment_with_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatoldbadcred = open_stream(&state, &open_req(&repo, "seatoldbadcred"))
            .unwrap()
            .context
            .stream_id;

        crate::state::append(
            &state,
            &id_seatoldbadcred,
            "human",
            SEAT_ASSIGNED_FAMILY,
            "not valid json at all".to_string(),
            vec![],
        )
        .unwrap();
        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatoldbadcred, Seat::Orchestrator, "claude")),
        )
        .await
        .unwrap();

        let (_guard, secrets_path) = isolated_secrets_path();
        secrets::test_support::with_isolated_credentials(&secrets_path, || {
            let mut s = secrets::SecretsFile::default();
            s.anthropic_api_key = Some("test-only-api-key-not-a-real-credential".into());
            secrets::save_to(&secrets_path, &s).unwrap();
            assert_eq!(
                resolve_seat(&state, &id_seatoldbadcred, Seat::Orchestrator)
                    .unwrap()
                    .name(),
                "claude"
            );
        });
    }

    #[tokio::test]
    async fn reassigning_a_seat_invalidates_the_cached_resolution() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatcache = open_stream(&state, &open_req(&repo, "seatcache"))
            .unwrap()
            .context
            .stream_id;

        // Seed a distinguishable resolved entry directly, bypassing the Heart
        // (the way a real "grok" resolution would have been cached).
        crate::state::seed_seat_model_for_test(
            &state,
            &id_seatcache,
            Seat::Orchestrator,
            Arc::new(ScriptedModel::new(&[])),
        );
        assert_eq!(
            resolve_seat(&state, &id_seatcache, Seat::Orchestrator)
                .unwrap()
                .name(),
            "scripted"
        );

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_seatcache, Seat::Orchestrator, "claude")),
        )
        .await
        .unwrap();

        // Whatever this resolves to now (Ok with real credentials, Err
        // without) it must no longer be the stale cached double — proof the
        // reassignment actually evicted it rather than leaving it live.
        match resolve_seat(&state, &id_seatcache, Seat::Orchestrator) {
            Ok(m) => assert_ne!(
                m.name(),
                "scripted",
                "must not keep answering with the old cached model"
            ),
            Err(e) => assert!(!e.to_ascii_lowercase().contains("scripted"), "{e}"),
        }
    }

    /// A switch of a Claude seat's provider asked for while its turn runs
    /// waits for that turn to end: until then the seat's assignment, the
    /// provider its sign-in is swapped for at the door (`swap.rs` reads
    /// both) and its model all stay as they are. A second switch replaces
    /// the first, one back to where the seat is cancels it, a switch to
    /// another runtime waits the same way, and the turn's end lands the one
    /// waiting before it lets the seat go. While idle, a switch lands at
    /// once. One a restart left waiting lands as the next turn starts.
    #[tokio::test]
    async fn a_switch_asked_for_mid_turn_lands_when_the_turn_ends() {
        use crate::claude_providers::{for_seat, Provider};
        use crate::node::{SeatSwitch, SEAT_SWITCH_FAMILY};
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let sid = open_stream(&state, &open_req(&repo, "seatswitch")).unwrap().context.stream_id;
        let seat = Seat::Orchestrator;
        let switch = |model: &str, provider: Option<&str>| {
            let req = SeatAssignReq { provider: provider.map(str::to_string), ..req(&sid, seat, model) };
            assign_seat(State(state.clone()), Json(req))
        };
        let assigned = || crate::state::latest_seat_model_id(&state.heart.lock().unwrap(), &sid, seat).unwrap();
        let waiting = || {
            let heart = state.heart.lock().unwrap();
            crate::state::seat_switch_waiting_locked(&heart, &sid, seat).map(|(_, to)| (to.model_id, to.provider))
        };
        let native = |provider: &str| Some(("claude-native".to_string(), Some(provider.to_string())));
        let idle = switch("claude-native", Some("subscription")).await.unwrap();
        assert_eq!(idle.family, SEAT_ASSIGNED_FAMILY, "idle: at once");

        let turn = crate::state::admit_turn(&state, &sid, seat).unwrap();
        crate::state::seed_seat_model_for_test(&state, &sid, seat, Arc::new(ScriptedModel::new(&[])));
        assert_eq!(switch("claude-native", Some("console")).await.unwrap().family, SEAT_SWITCH_FAMILY);
        assert_eq!(waiting(), native("console"));
        assert_eq!(assigned().as_deref(), Some("claude-native"));
        assert_eq!(for_seat(&state, &sid, seat), Provider::Subscription, "the door keeps serving the running process");
        assert_eq!(resolve_seat(&state, &sid, seat).unwrap().name(), "scripted", "its model is never revoked under it");
        assert_eq!(switch("claude-native", Some("openrouter")).await.unwrap().family, SEAT_SWITCH_FAMILY);
        assert_eq!(waiting(), native("openrouter"), "a second switch replaces the first");
        assert_eq!(switch("claude-native", None).await.unwrap().family, SEAT_SWITCH_FAMILY);
        assert_eq!(waiting(), None, "back to the subscription it has cancels it");
        let unchanged = switch("claude-native", Some("subscription")).await.unwrap();
        assert_eq!(unchanged.event_hash, idle.event_hash, "nothing to cancel: the seat's own assignment, nothing new");
        assert!(switch("grok", Some("console")).await.is_err(), "checked when asked, not when it lands");
        assert_eq!(switch("grok", None).await.unwrap().family, SEAT_SWITCH_FAMILY, "another runtime waits too");
        assert_eq!(switch("claude-native", Some("console")).await.unwrap().family, SEAT_SWITCH_FAMILY);
        assert!(crate::state::is_turn_running(&state, &sid, seat));
        drop(turn);
        assert_eq!((assigned().as_deref(), waiting()), (Some("claude-native"), None), "the turn's end lands it");
        assert_eq!(for_seat(&state, &sid, seat), Provider::Console);
        assert!(!crate::state::is_turn_running(&state, &sid, seat), "then lets the seat go");
        match resolve_seat(&state, &sid, seat) {
            Ok(model) => assert_ne!(model.name(), "scripted"),
            Err(e) => assert!(!e.contains("scripted"), "{e}"),
        }

        // A restart ended the turn without its lease: the switch still waits.
        let over = state.heart.lock().unwrap().last_of_family(&sid, SEAT_ASSIGNED_FAMILY).unwrap().unwrap().event_hash;
        let left = SeatSwitch { seat, model_id: "stub".into(), provider: None, reason: None, over: Some(over) };
        crate::state::append(&state, &sid, "human", SEAT_SWITCH_FAMILY, serde_json::to_string(&left).unwrap(), vec![]).unwrap();
        assert_eq!(waiting(), Some(("stub".to_string(), None)));
        let next = crate::state::admit_turn(&state, &sid, seat).unwrap();
        assert_eq!((assigned().as_deref(), waiting()), (Some("stub"), None), "the next turn starts on it");

        // A switch read to land, then replaced before it lands: it doesn't
        // land, and the one that replaced it still waits for the lease.
        assert_eq!(switch("grok", None).await.unwrap().family, SEAT_SWITCH_FAMILY);
        let read = state.heart.lock().unwrap().last_of_family(&sid, SEAT_SWITCH_FAMILY).unwrap().unwrap();
        assert_eq!(switch("claude-native", Some("console")).await.unwrap().family, SEAT_SWITCH_FAMILY);
        let stale = Switch { model_id: "grok", provider: None, reason: None, because: None };
        let refused = seat_switch(&state, &sid, seat, &stale, Landing::Waiting(&read.event_hash)).unwrap_err();
        assert_eq!(refused.0, StatusCode::CONFLICT, "{refused:?}");
        assert_eq!((assigned().as_deref(), waiting()), (Some("stub"), native("console")), "the newer one still waits");
        drop(next);
        assert_eq!((assigned().as_deref(), waiting()), (Some("claude-native"), None), "and the turn's end lands it");

        // One a restart left waiting, cancelled while idle: the cancel is
        // recorded and the seat keeps its assignment (and its process).
        let kept = state.heart.lock().unwrap().last_of_family(&sid, SEAT_ASSIGNED_FAMILY).unwrap().unwrap().event_hash;
        let left = SeatSwitch { seat, model_id: "grok".into(), provider: None, reason: None, over: Some(kept.clone()) };
        crate::state::append(&state, &sid, "human", SEAT_SWITCH_FAMILY, serde_json::to_string(&left).unwrap(), vec![]).unwrap();
        assert_eq!(waiting(), Some(("grok".to_string(), None)));
        assert_eq!(switch("claude-native", Some("console")).await.unwrap().family, SEAT_SWITCH_FAMILY, "idle, a cancel");
        assert_eq!(waiting(), None);
        let now = state.heart.lock().unwrap().last_of_family(&sid, SEAT_ASSIGNED_FAMILY).unwrap().unwrap().event_hash;
        assert_eq!(now, kept, "no new assignment");

        // One that can't land: tried, then recorded as failed on its last
        // try. It waits no more, the seat stays, and the failure shows until
        // another switch moves the seat.
        let failed = || {
            let heart = state.heart.lock().unwrap();
            crate::state::seat_switch_failed_locked(&heart, &sid, seat).map(|(_, to, why)| (to.model_id, why))
        };
        let bad = SeatSwitch { seat, model_id: "grok".into(), provider: Some("console".into()), reason: None, over: Some(kept.clone()) };
        crate::state::append(&state, &sid, "human", SEAT_SWITCH_FAMILY, serde_json::to_string(&bad).unwrap(), vec![]).unwrap();
        switch_waiting(&state, &sid, seat, false);
        assert_eq!((waiting(), failed()), (Some(("grok".to_string(), Some("console".to_string()))), None), "not its last try");
        let next = crate::state::admit_turn(&state, &sid, seat).unwrap();
        let (to, why) = failed().expect("recorded as failed");
        assert!(to == "grok" && why.contains("doesn't take one"), "{why}");
        assert_eq!((waiting(), assigned().as_deref()), (None, Some("claude-native")), "it waits no more; the seat stays");
        drop(next);
        assert_eq!(switch("grok", None).await.unwrap().family, SEAT_ASSIGNED_FAMILY);
        assert_eq!(failed(), None, "another switch moved the seat");
    }

    #[tokio::test]
    async fn assign_seat_refuses_while_the_stream_is_mid_discard() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_seatretire = open_stream(&state, &open_req(&repo, "seatretire"))
            .unwrap()
            .context
            .stream_id;
        set_non_burn_lifetime(&state, &id_seatretire, Lifetime::Slice).unwrap();

        let (_guard, _prior) = begin_discard(&state, &id_seatretire, false).unwrap();
        let err = assign_seat(
            State(state),
            Json(req(&id_seatretire, Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
    }

    /// 07d-A1 §4: Baker is factory-only — a project socket must never accept
    /// a baker seat assignment.
    #[tokio::test]
    async fn assign_seat_refuses_baker_on_a_project_socket() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_notbaker = open_stream(&state, &open_req(&repo, "notbaker"))
            .unwrap()
            .context
            .stream_id;

        let err = assign_seat(State(state), Json(req(&id_notbaker, Seat::Baker, "grok")))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.to_ascii_lowercase().contains("project socket"),
            "{}",
            err.1
        );
    }

    /// The mirror case: the factory socket must never accept an ordinary
    /// project seat assignment (orchestrator/worker).
    #[tokio::test]
    async fn assign_seat_refuses_a_project_seat_on_the_factory_socket() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_baker_fac = open_stream(&state, &open_req(&repo, "baker-fac"))
            .unwrap()
            .context
            .stream_id;
        mark_factory(&state, &id_baker_fac);

        let err = assign_seat(
            State(state.clone()),
            Json(req(&id_baker_fac, Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.to_ascii_lowercase().contains("factory socket"),
            "{}",
            err.1
        );

        let err = assign_seat(State(state), Json(req(&id_baker_fac, Seat::Worker, "grok")))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
    }

    /// The valid pairing on each socket kind must still work: baker on the
    /// factory socket, and an ordinary seat on a project socket.
    #[tokio::test]
    async fn assign_seat_allows_the_matching_seat_on_each_socket_kind() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        let id_baker_ok = open_stream(&state, &open_req(&repo, "baker-ok"))
            .unwrap()
            .context
            .stream_id;
        let id_projok = open_stream(&state, &open_req(&repo, "projok"))
            .unwrap()
            .context
            .stream_id;
        mark_factory(&state, &id_baker_ok);

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_baker_ok, Seat::Baker, "grok")),
        )
        .await
        .unwrap();
        let _ = assign_seat(
            State(state),
            Json(req(&id_projok, Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap();
    }

    /// 07d-A1 correction: factory authority is a durable server fact, never
    /// inferred from a name/branch/stream_id convention. An ordinary
    /// project a human happens to name `baker-...` (no
    /// `socket.kind.recorded` fact — never went through `/baker/assign`)
    /// must behave exactly like any other project socket: ordinary seats
    /// assign fine, and it must still refuse the baker seat.
    #[tokio::test]
    async fn a_baker_named_project_with_no_recorded_kind_is_not_treated_as_factory() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = state_with(dir.path(), Arc::new(StubModel));
        // Deliberately named to match the OLD name-based heuristic, and
        // deliberately never `mark_factory`'d — this must not spoof factory
        // authority.
        let id_baker_spoof = open_stream(&state, &open_req(&repo, "baker-spoof"))
            .unwrap()
            .context
            .stream_id;

        let _ = assign_seat(
            State(state.clone()),
            Json(req(&id_baker_spoof, Seat::Orchestrator, "grok")),
        )
        .await
        .unwrap();

        let err = assign_seat(
            State(state),
            Json(req(&id_baker_spoof, Seat::Baker, "grok")),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert!(
            err.1.to_ascii_lowercase().contains("project socket"),
            "{}",
            err.1
        );
    }
}
