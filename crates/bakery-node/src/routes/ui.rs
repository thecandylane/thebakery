//! The app's own state, kept by the node in its home so it outlives the app's
//! origin and every window starts from it: the shell's layout
//! (bakery-client/src/lib/react/shell/layout.ts) and the appearance
//! (src/lib/themeModel.ts). The Baker can read and change both (ui_layout,
//! ui_appearance and their _set tools, graded write). Each is stored as the
//! JSON the app sent, so a field this node doesn't know yet survives a save;
//! a save goes out over /events as `{"kind":"ui-layout"|"ui-appearance",…}`
//! so an open app follows it.

use crate::state::AppState;
use axum::{extract::State, http::StatusCode, Json};
use serde::Deserialize;
use serde_json::Value;

/// Plenty for a layout or an appearance; a bigger one is a mistake.
const MAX_BYTES: usize = 64 * 1024;

/// layout.ts's `Layout`, version 1: workspaces, each a strip of columns,
/// each column a stack of panels, plus the Baker's own workspace and the
/// sidebar. A layout saved with the old Baker `dock` still loads.
#[derive(schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub(crate) struct Layout {
    /// Always 1.
    version: u32,
    /// Which side the window shows; bakery when unset.
    #[serde(default)]
    mode: Option<Mode>,
    /// The Bakery workspace shown in bakery mode.
    #[serde(default)]
    active: Option<String>,
    workspaces: Vec<Workspace>,
    /// The Baker's own workspace (id "baker"): its threads and what it opens.
    #[serde(default)]
    baker: Option<Workspace>,
    /// Projects on the Baker's side (the Bakery's own repo): shown in baker
    /// mode, with no Bakery workspace of their own.
    #[serde(default)]
    baker_projects: Vec<String>,
    sidebar: Sidebar,
    /// The bottom panel: the focused agent's terminals.
    #[serde(default)]
    bottom: Option<Bottom>,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[allow(dead_code)]
struct Bottom {
    open: bool,
    height: f64,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum Mode {
    /// The operator's workspaces.
    Bakery,
    /// The Baker's own workspace.
    Baker,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Workspace {
    id: String,
    name: String,
    /// Set for the one-per-project workspaces; null for one made by hand.
    #[serde(default)]
    project_id: Option<String>,
    columns: Vec<Column>,
    /// The focused panel.
    #[serde(default)]
    focus: Option<String>,
    /// How it lays out its columns; screen when unset.
    #[serde(default)]
    view: Option<View>,
    /// Panels locked open: they show no close button and don't close.
    #[serde(default)]
    locked: Vec<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum View {
    /// Each column is a group of tabs; the first takes the room left.
    Screen,
    /// A horizontal strip of columns, each a stack of panels.
    Strip,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[allow(dead_code)]
struct Column {
    id: String,
    width: f64,
    /// Panel ids, top to bottom: a conversation's stream id, or "@review",
    /// "@containers", "@code:<stream>", "@preview:<stream>".
    panels: Vec<String>,
    /// The tab it shows in the screen view.
    #[serde(default)]
    active: Option<String>,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
enum SidebarMode {
    Agents,
    Worktrees,
}

#[derive(schemars::JsonSchema, Deserialize)]
#[allow(dead_code)]
struct Sidebar {
    open: bool,
    width: f64,
    /// What it lists; agents when unset.
    #[serde(default)]
    mode: Option<SidebarMode>,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SaveLayoutReq {
    #[schemars(with = "Layout")]
    pub(crate) layout: Value,
}

#[derive(schemars::JsonSchema, Deserialize)]
pub(crate) struct SaveAppearanceReq {
    /// The app's AppearanceState (version 1: selected, schemes), stored as
    /// given.
    pub(crate) appearance: Value,
}

/// One of the app's states, by name: its file in the node's home, and the
/// event kind a save broadcasts.
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Layout,
    Appearance,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Layout => "layout",
            Kind::Appearance => "appearance",
        }
    }
}

/// The saved state, or none until the app (or the Baker) saves one.
pub(crate) fn load(state: &AppState, kind: Kind) -> Option<Value> {
    let text = std::fs::read_to_string(state.base.join(format!("ui-{}.json", kind.name()))).ok()?;
    serde_json::from_str(&text).ok()
}

/// Checks and stores it, atomically, then tells open apps.
pub(crate) fn save(state: &AppState, kind: Kind, value: Value) -> Result<Value, String> {
    let text = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    if text.len() > MAX_BYTES {
        return Err(format!("the {} is {} bytes; at most {MAX_BYTES}", kind.name(), text.len()));
    }
    if value["version"] != 1 {
        return Err(format!("the {} must be version 1, as this node knows it", kind.name()));
    }
    if let Kind::Layout = kind {
        serde_json::from_value::<Layout>(value.clone()).map_err(|e| format!("not a layout: {e}"))?;
    }
    let path = state.base.join(format!("ui-{}.json", kind.name()));
    let tmp = state.base.join(format!("ui-{}.json.{}", kind.name(), crate::node::mint_id("tmp")));
    std::fs::write(&tmp, &text)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("could not save the {}: {e}", kind.name())
        })?;
    let event = match kind {
        Kind::Layout => serde_json::json!({"kind": "ui-layout", "layout": value}),
        Kind::Appearance => serde_json::json!({"kind": "ui-appearance", "appearance": value}),
    };
    let _ = state.tx.send(event.to_string());
    Ok(value)
}

type Reply = Result<Json<Value>, (StatusCode, String)>;

async fn stored(state: AppState, kind: Kind) -> Json<Option<Value>> {
    Json(crate::actor::spawn_blocking(move || load(&state, kind)).await.unwrap_or(None))
}

async fn store(state: AppState, kind: Kind, value: Value) -> Reply {
    crate::actor::spawn_blocking(move || save(&state, kind, value))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

pub(crate) async fn layout_route(State(st): State<AppState>) -> Json<Option<Value>> {
    stored(st, Kind::Layout).await
}

pub(crate) async fn save_layout_route(State(st): State<AppState>, Json(req): Json<SaveLayoutReq>) -> Reply {
    store(st, Kind::Layout, req.layout).await
}

pub(crate) async fn appearance_route(State(st): State<AppState>) -> Json<Option<Value>> {
    stored(st, Kind::Appearance).await
}

pub(crate) async fn save_appearance_route(State(st): State<AppState>, Json(req): Json<SaveAppearanceReq>) -> Reply {
    store(st, Kind::Appearance, req.appearance).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    #[test]
    fn the_layout_and_appearance_round_trip_as_sent_and_open_apps_hear_of_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        assert!(load(&state, Kind::Layout).is_none(), "none until one is saved");
        let mut events = state.tx.subscribe();
        let layout = serde_json::json!({
            "version": 1,
            "active": "project:p",
            "workspaces": [{"id": "project:p", "name": "P", "projectId": "p", "focus": "wt:a",
                "columns": [{"id": "c1", "width": 560, "panels": ["wt:a", "@preview:wt:a"]}]}],
            "sidebar": {"open": true, "width": 260, "mode": "worktrees"},
            "dock": {"open": true, "width": 420, "streamId": null},
            "addedLater": {"kept": true},
        });
        save(&state, Kind::Layout, layout.clone()).unwrap();
        assert_eq!(load(&state, Kind::Layout), Some(layout.clone()), "stored as sent, unknown fields kept");
        let heard: Value = serde_json::from_str(&events.try_recv().unwrap()).unwrap();
        assert_eq!((heard["kind"].as_str(), &heard["layout"]), (Some("ui-layout"), &layout));

        let refused = |kind, value: Value| save(&state, kind, value).unwrap_err();
        assert!(refused(Kind::Layout, serde_json::json!({"version": 2})).contains("version 1"));
        assert!(refused(Kind::Layout, serde_json::json!({"version": 1, "workspaces": "x"})).contains("not a layout"));
        let huge = "x".repeat(MAX_BYTES);
        assert!(refused(Kind::Appearance, serde_json::json!({"version": 1, "selected": huge})).contains("at most"));
        assert_eq!(load(&state, Kind::Layout), Some(layout), "a refused save changes nothing");

        // Baker mode: the Baker's own workspace and the projects on its side.
        let baker = serde_json::json!({
            "version": 1, "mode": "baker", "active": null, "workspaces": [],
            "baker": {"id": "baker", "name": "Baker", "columns": [{"id": "c", "width": 560, "panels": ["b:main"]}]},
            "bakerProjects": ["p"],
            "sidebar": {"open": true, "width": 272},
        });
        save(&state, Kind::Layout, baker).unwrap();
        let odd_mode = serde_json::json!({"version": 1, "mode": "both", "workspaces": [], "sidebar": {"open": true, "width": 1}});
        assert!(refused(Kind::Layout, odd_mode).contains("not a layout"));

        // The appearance is a blob the node doesn't read.
        let appearance = serde_json::json!({"version": 1, "selected": "auto", "schemes": [{"id": "warm"}]});
        save(&state, Kind::Appearance, appearance.clone()).unwrap();
        assert_eq!(load(&state, Kind::Appearance), Some(appearance));
    }
}
