//! Which provider a native Claude seat runs on: the operator's Claude
//! subscription (today's sign-in, unchanged), their own Anthropic console
//! API key, or their OpenRouter key. The choice is per seat (recorded with
//! its model assignment, `node::SeatAssignment::provider`), not a second
//! model id — every existing check that asks "is this seat's model
//! `claude-native`" (agents.rs, seat_container.rs, state.rs, move_in.rs…)
//! stays exactly as it was; only the native engine's own spawn (`swap.rs`,
//! `claude_native.rs`) reads which provider that seat is on.
//!
//! Keys live with the node only, the way the subscription does: a seat
//! holds a per-thread minted stand-in (`crate::swap`), never the real key;
//! the door swaps it for the real one, toward that provider's own host
//! only. `auto` never selects one of these — it is not a model id a seat
//! resolves through [`crate::state::resolve_model_id_for_seat`]'s plain
//! `bakery_agent::resolve_seat_model` path at all.

use crate::node::{Seat, SeatAssignment, SEAT_ASSIGNED_FAMILY};
use crate::state::AppState;

/// Which upstream a native Claude seat's requests are swapped toward.
/// [`Default`] (and every assignment that names no provider) is
/// `Subscription` — today's sign-in, unchanged for every thread that
/// predates this choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Provider {
    #[default]
    Subscription,
    Console,
    OpenRouter,
}

impl Provider {
    /// The id a `SeatAssignment.provider` and a `/seat/assign` request use.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Provider::Subscription => "subscription",
            Provider::Console => "console",
            Provider::OpenRouter => "openrouter",
        }
    }

    /// What the picker shows beside the model name, e.g. "Opus 5.5 · console".
    pub(crate) fn label(self) -> &'static str {
        match self {
            Provider::Subscription => "subscription",
            Provider::Console => "console",
            Provider::OpenRouter => "OpenRouter",
        }
    }

    pub(crate) fn parse(id: &str) -> Option<Provider> {
        match id.trim().to_ascii_lowercase().as_str() {
            "subscription" => Some(Provider::Subscription),
            "console" => Some(Provider::Console),
            "openrouter" => Some(Provider::OpenRouter),
            _ => None,
        }
    }
}

/// The provider this seat's newest assignment named, or [`Provider::default`]
/// when none did (no assignment yet, an assignment from before this choice
/// existed, or one that left the field out because the operator picked
/// nothing beyond the model) — same fallback shape as
/// `state::latest_seat_model_id`'s own newest-record scan.
pub(crate) fn for_seat(state: &AppState, stream_id: &str, seat: Seat) -> Provider {
    let heart = state.heart.lock().unwrap_or_else(|e| e.into_inner());
    for_seat_locked(&heart, stream_id, seat)
}

/// [`for_seat`]'s scan, for a caller that already holds `state.heart`'s lock
/// (`Mutex` isn't reentrant — re-locking on the same thread would deadlock).
/// `crate::swap::Subscription::on_thread` is the one such caller: it checks
/// a seat's model AND its provider under one lock, so the two can never
/// disagree about which row currently governs that seat.
pub(crate) fn for_seat_locked(heart: &bakery_heart::Heart, stream_id: &str, seat: Seat) -> Provider {
    let Ok(events) = heart.events_of_family(stream_id, SEAT_ASSIGNED_FAMILY) else {
        return Provider::default();
    };
    for ev in events.iter().rev() {
        let Ok(a) = serde_json::from_str::<SeatAssignment>(&ev.payload) else {
            continue;
        };
        if a.seat == seat {
            return a
                .provider
                .as_deref()
                .and_then(Provider::parse)
                .unwrap_or_default();
        }
    }
    Provider::default()
}

/// Whether `provider` may be recorded alongside `model_id`: only Claude
/// native takes one. `None` (no provider named) is always fine, whatever
/// the model — the normalized id to store, or the plain error a `/seat/
/// assign` caller shows.
pub(crate) fn validate(model_id: &str, provider: Option<&str>) -> Result<Option<String>, String> {
    let Some(given) = provider.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if !model_id.trim().eq_ignore_ascii_case(crate::claude_native::ACCOUNT_ID) {
        return Err(format!(
            "a provider is Claude native's own setting — model '{}' doesn't take one",
            model_id.trim()
        ));
    }
    let parsed = Provider::parse(given)
        .ok_or_else(|| format!("'{given}' isn't a provider: subscription, console, or openrouter"))?;
    Ok(Some(parsed.id().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use std::sync::Arc;

    fn seat_state(dir: &std::path::Path) -> AppState {
        crate::testutil::state_with(dir, Arc::new(bakery_agent::StubModel))
    }

    #[test]
    fn validate_only_lets_claude_native_take_a_provider() {
        assert_eq!(validate("claude-native", None).unwrap(), None);
        assert_eq!(validate("claude-native", Some("console")).unwrap(), Some("console".to_string()));
        assert_eq!(validate("claude-native", Some(" OpenRouter ")).unwrap(), Some("openrouter".to_string()));
        assert!(validate("claude-native", Some("bogus")).unwrap_err().contains("isn't a provider"));
        assert!(validate("grok", Some("console")).unwrap_err().contains("Claude native's own setting"));
        assert_eq!(validate("grok", None).unwrap(), None);
    }

    #[test]
    fn a_seats_newest_named_provider_wins_and_an_unnamed_one_defaults_to_subscription() {
        let dir = tempfile::tempdir().unwrap();
        let repo = make_repo(&dir.path().join("repo"));
        let state = seat_state(dir.path());
        let thread = crate::routes::project::open_stream(&state, &open_req(&repo, "providers")).unwrap().context.stream_id;

        assert_eq!(for_seat(&state, &thread, Seat::Orchestrator), Provider::Subscription, "no assignment yet");

        crate::state::assign_seat_model(&state, &thread, Seat::Orchestrator, "claude-native").unwrap();
        assert_eq!(for_seat(&state, &thread, Seat::Orchestrator), Provider::Subscription, "an assignment with no provider named");

        let assign = |seat: Seat, model: &str, provider: Option<&str>| {
            crate::state::assign_seat_model_for(&state, &thread, seat, model, None, None, provider).unwrap();
        };
        assign(Seat::Orchestrator, "claude-native", Some("console"));
        assert_eq!(for_seat(&state, &thread, Seat::Orchestrator), Provider::Console);

        // A second seat's choice never leaks into the first's.
        assign(Seat::Worker, "claude-native", Some("openrouter"));
        assert_eq!(for_seat(&state, &thread, Seat::Worker), Provider::OpenRouter);
        assert_eq!(for_seat(&state, &thread, Seat::Orchestrator), Provider::Console, "unaffected by the other seat");

        // Switching back to subscription (named explicitly) really switches.
        assign(Seat::Orchestrator, "claude-native", Some("subscription"));
        assert_eq!(for_seat(&state, &thread, Seat::Orchestrator), Provider::Subscription);
    }
}
