//! What a conversation's selector shows, its model and effort, which every
//! one of its turns runs on: the operator's, a queued one, one another thread
//! sent, one carried on after a restart. Nothing else sets them.
//!
//! They are kept as the setting records the native picker writes
//! ([`NATIVE_CONFIG_ACKNOWLEDGED`]), so every change shows in the thread as
//! "Model set to …". A native pick names its session's own option
//! (`config_id`); a choice made with no session to ask (thread_create, the
//! composer of the Bakery's own loop) names its kind (`category`), and a
//! native session matches it to its option when it starts
//! (`pipe::restore_durable_native_config`).

use crate::node::Seat;
use crate::routes::agent::NATIVE_CONFIG_ACKNOWLEDGED;
use crate::state::{append, AppState};
use bakery_heart::Heart;
use serde::{Deserialize, Serialize};

/// The kinds of choice a selector makes, as ACP names its option categories.
pub(crate) const MODEL: &str = "model";
pub(crate) const EFFORT: &str = "thought_level";

/// A selector's model and effort, by kind; `None` is the runtime's own.
#[derive(schemars::JsonSchema, Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Selection {
    #[serde(default)]
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) effort: Option<String>,
}

/// The newest choice of each kind for `stream_id`'s `seat` on `runtime`.
pub(crate) fn chosen_locked(heart: &Heart, stream_id: &str, seat: Seat, runtime: &str) -> Selection {
    let agent_id = crate::node::agent_id(stream_id, seat);
    let events = heart.events_of_family(stream_id, NATIVE_CONFIG_ACKNOWLEDGED).unwrap_or_default();
    let (mut model, mut effort) = (None, None);
    for event in events.iter().rev() {
        let Ok(p) = serde_json::from_str::<serde_json::Value>(&event.payload) else {
            continue;
        };
        if p["agent_id"] != agent_id.as_str() || p["account_id"] != runtime {
            continue;
        }
        let value = p["effective"].as_str().map(str::to_string);
        match p["category"].as_str() {
            Some(MODEL) if model.is_none() => model = Some(value),
            Some(EFFORT) if effort.is_none() => effort = Some(value),
            _ => {}
        }
        if model.is_some() && effort.is_some() {
            break;
        }
    }
    Selection { model: model.flatten(), effort: effort.flatten() }
}

/// [`chosen_locked`] on the runtime the seat runs on now.
pub(crate) fn chosen(state: &AppState, stream_id: &str, seat: Seat) -> Selection {
    let heart = state.heart.lock().unwrap();
    let runtime = crate::state::latest_seat_model_id(&heart, stream_id, seat).ok().flatten().unwrap_or_default();
    chosen_locked(&heart, stream_id, seat, &runtime)
}

/// Sets the selector's `category` choice on `runtime` to `value` (`None`:
/// the runtime's own), shown as `label`. Nothing when it is already that.
pub(crate) fn choose(
    state: &AppState,
    stream_id: &str,
    seat: Seat,
    runtime: &str,
    category: &str,
    value: Option<&str>,
    label: Option<&str>,
    actor: &str,
) -> Result<(), String> {
    let value = crate::state::normalize_override(value);
    let current = {
        let heart = state.heart.lock().unwrap();
        chosen_locked(&heart, stream_id, seat, runtime)
    };
    let was = if category == MODEL { current.model } else { current.effort };
    if was.as_deref() == value {
        return Ok(());
    }
    let fact = serde_json::json!({
        "agent_id": crate::node::agent_id(stream_id, seat),
        "seat": seat,
        "account_id": runtime,
        "category": category,
        "name": if category == MODEL { "Model" } else { "Effort" },
        "requested": value,
        "effective": value,
        "value_name": label.or(value).unwrap_or("Default"),
    });
    append(state, stream_id, actor, NATIVE_CONFIG_ACKNOWLEDGED, fact.to_string(), vec![]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{state_with, StubModel};
    use std::sync::Arc;

    /// Each choice is one row the thread shows, the newest of each kind is
    /// what the selector holds, a repeat writes nothing, and a choice belongs
    /// to the runtime it was made on.
    #[test]
    fn the_newest_choice_of_each_kind_on_its_runtime_is_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let (sid, seat) = ("wt:selection", Seat::Orchestrator);
        let rows = || state.heart.lock().unwrap().events_of_family(sid, NATIVE_CONFIG_ACKNOWLEDGED).unwrap();
        let chosen = |runtime: &str| chosen_locked(&state.heart.lock().unwrap(), sid, seat, runtime);

        choose(&state, sid, seat, "grok", MODEL, Some("grok-4"), Some("Grok 4"), "human").unwrap();
        choose(&state, sid, seat, "grok", EFFORT, Some("low"), None, "human").unwrap();
        choose(&state, sid, seat, "grok", EFFORT, Some("high"), Some("High"), "human").unwrap();
        choose(&state, sid, seat, "grok", EFFORT, Some(" high "), None, "human").unwrap();
        assert_eq!(rows().len(), 3, "the repeat wrote nothing");
        assert_eq!(chosen("grok"), Selection { model: Some("grok-4".into()), effort: Some("high".into()) });
        let shown: serde_json::Value = serde_json::from_str(&rows()[2].payload).unwrap();
        assert_eq!((shown["name"].as_str(), shown["value_name"].as_str()), (Some("Effort"), Some("High")));
        assert_eq!(chosen("claude"), Selection::default(), "another runtime's choice is not this one's");

        choose(&state, sid, seat, "grok", EFFORT, None, None, "human").unwrap();
        assert_eq!(chosen("grok").effort, None, "back to the runtime's own");
        let shown: serde_json::Value = serde_json::from_str(&rows()[3].payload).unwrap();
        assert_eq!(shown["value_name"], "Default");
    }
}
