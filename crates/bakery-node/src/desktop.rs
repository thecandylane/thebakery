//! The desktop app's link: agents drive a conversation's own previews
//! through it. Bakery's Electron shell keeps one WebSocket here
//! (`/desktop/ws`, behind a one-time ticket the operator credential mints at
//! `/events/ticket`), says which preview views it has open for which
//! conversation, and runs each command relayed to it on that one view
//! (bakery-client/electron/link.ts). The app window is never driven: the
//! shell registers only the views it made, and both sides check that a
//! command's view belongs to the conversation it names. An agent names only
//! its own conversation ([`own_previews_only`]). The window is only ever
//! captured, whole, for a screenshot ([`screenshot_route`]); its pixels are
//! never kept, only handed to the asking agent's model.

use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// The tools that act in the desktop app, each for the caller's own
/// conversation: the previews', and the window's screenshot.
pub(crate) const PREVIEW_TOOLS: [&str; 6] =
    ["preview_snapshot", "preview_navigate", "preview_click", "preview_eval", "preview_open", "desktop_screenshot"];
/// Each screenshot taken, in the asking conversation: who and when, never the pixels.
pub(crate) const SCREENSHOT_FAMILY: &str = "desktop.screenshot.taken";
const ANSWER_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Deserialize)]
struct Preview {
    view_id: String,
    stream_id: String,
}

struct Link {
    id: u64,
    tx: mpsc::UnboundedSender<String>,
    previews: Vec<Preview>,
}

// ponytail: one desktop app per node, in memory. A second connection
// replaces the first; after a node restart the app reconnects on its own.
fn link() -> &'static Mutex<Option<Link>> {
    static LINK: OnceLock<Mutex<Option<Link>>> = OnceLock::new();
    LINK.get_or_init(Default::default)
}

type Answer = oneshot::Sender<Result<Value, String>>;

fn waiting() -> &'static Mutex<HashMap<String, Answer>> {
    static WAITING: OnceLock<Mutex<HashMap<String, Answer>>> = OnceLock::new();
    WAITING.get_or_init(Default::default)
}

/// An agent drives its own conversation's previews only: a preview tool's
/// `stream_id` is the caller's, filled in when left out, never another's.
pub(crate) fn own_previews_only(caller_stream: &str, tool: &str, args: &mut Value) -> Result<(), String> {
    if !PREVIEW_TOOLS.contains(&tool) {
        return Ok(());
    }
    if args.is_null() {
        *args = json!({});
    }
    let Some(args) = args.as_object_mut() else {
        return Ok(());
    };
    match args.get("stream_id").and_then(Value::as_str) {
        None | Some("") => {
            args.insert("stream_id".into(), json!(caller_stream));
            Ok(())
        }
        Some(named) if named == caller_stream => Ok(()),
        Some(other) => Err(format!("{tool} drives this conversation's own previews only, not {other}'s")),
    }
}

#[derive(Deserialize)]
pub(crate) struct LinkQuery {
    #[serde(default)]
    ticket: String,
}

/// `GET /desktop/ws`: the desktop app's link, for an allowed origin (or a
/// caller with none) holding a ticket minted behind operator auth.
pub(crate) async fn link_ws(
    ws: WebSocketUpgrade,
    Query(q): Query<LinkQuery>,
    headers: HeaderMap,
    State(st): State<AppState>,
) -> Response {
    if !crate::state::origin_allowed(&headers) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    if !crate::state::consume_events_ticket(&st, &q.ticket) {
        return (StatusCode::FORBIDDEN, "missing, unknown, or expired ticket — mint one via POST /events/ticket")
            .into_response();
    }
    ws.on_upgrade(serve).into_response()
}

async fn serve(mut socket: WebSocket) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    *link().lock().unwrap() = Some(Link { id, tx, previews: Vec::new() });
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                Some(text) => {
                    if socket.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                // Replaced by a newer connection.
                None => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => heard(id, &text),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    let mut current = link().lock().unwrap();
    if current.as_ref().is_some_and(|l| l.id == id) {
        *current = None;
    }
}

/// What the app says: the previews it has open, or a command's answer.
fn heard(id: u64, text: &str) {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return;
    };
    match message["type"].as_str() {
        Some("previews") => {
            let previews: Vec<Preview> = serde_json::from_value(message["previews"].clone()).unwrap_or_default();
            if let Some(link) = link().lock().unwrap().as_mut().filter(|l| l.id == id) {
                link.previews = previews;
            }
        }
        Some("result") => {
            let Some(done) = message["id"].as_str().and_then(|r| waiting().lock().unwrap().remove(r)) else {
                return;
            };
            let answer = if message["ok"] == true {
                Ok(message["value"].clone())
            } else {
                Err(message["error"].as_str().unwrap_or("the preview failed").to_string())
            };
            let _ = done.send(answer);
        }
        _ => {}
    }
}

/// Runs `op` on `stream_id`'s preview (`view_id`, else the one opened last)
/// in the desktop app and waits for its answer.
async fn drive(stream_id: &str, view_id: Option<&str>, op: &str, args: Value) -> Result<Value, String> {
    if stream_id.trim().is_empty() {
        return Err("which conversation's preview? stream_id is empty".to_string());
    }
    let (tx, view_id) = {
        let link = link().lock().unwrap();
        let link = link.as_ref().ok_or(
            "Bakery's desktop app isn't connected, so no preview is open to drive: open the Bakery app and this conversation's Preview panel",
        )?;
        let mine: Vec<&Preview> = link.previews.iter().filter(|p| p.stream_id == stream_id).collect();
        let view = match view_id {
            Some(wanted) => mine
                .into_iter()
                .find(|p| p.view_id == wanted)
                .ok_or_else(|| format!("no preview {wanted} is open for this conversation"))?,
            None => *mine
                .last()
                .ok_or("no preview is open for this conversation: open its Preview panel in Bakery first")?,
        };
        (link.tx.clone(), view.view_id.clone())
    };
    relay(tx, json!({"op": op, "view_id": view_id, "stream_id": stream_id, "args": args})).await
}

/// Sends one command to the desktop app and waits for its answer.
async fn relay(tx: mpsc::UnboundedSender<String>, mut command: Value) -> Result<Value, String> {
    let request = crate::node::mint_id("preview");
    let (done, answer) = oneshot::channel();
    waiting().lock().unwrap().insert(request.clone(), done);
    command["id"] = json!(request);
    if tx.send(command.to_string()).is_err() {
        waiting().lock().unwrap().remove(&request);
        return Err("Bakery's desktop app just disconnected".to_string());
    }
    match tokio::time::timeout(ANSWER_WAIT, answer).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) => Err("Bakery's desktop app went away before answering".to_string()),
        Err(_) => {
            waiting().lock().unwrap().remove(&request);
            Err(format!("the desktop app did not answer within {}s", ANSWER_WAIT.as_secs()))
        }
    }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PreviewReq {
    /// The conversation whose preview it is: an agent's own, which is also
    /// the default.
    #[serde(default)]
    pub(crate) stream_id: String,
    /// Which of its open previews; the one opened last by default.
    #[serde(default)]
    pub(crate) view_id: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PreviewNavigateReq {
    #[serde(default)]
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) view_id: Option<String>,
    /// An http(s) URL.
    pub(crate) url: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PreviewClickReq {
    #[serde(default)]
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) view_id: Option<String>,
    /// A CSS selector; the first element it matches is clicked.
    pub(crate) selector: String,
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct PreviewEvalReq {
    #[serde(default)]
    pub(crate) stream_id: String,
    #[serde(default)]
    pub(crate) view_id: Option<String>,
    /// A JavaScript expression, awaited if it is a promise.
    pub(crate) expression: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PreviewPage {
    pub(crate) url: String,
    pub(crate) title: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PreviewSnapshot {
    pub(crate) url: String,
    pub(crate) title: String,
    /// The page's visible text, at most 20,000 characters.
    pub(crate) text: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct PreviewValue {
    /// The value as JSON, at most 20,000 characters, or "undefined".
    pub(crate) value: String,
}

fn answer<T: DeserializeOwned>(result: Result<Value, String>) -> Result<Json<T>, (StatusCode, String)> {
    let value = result.map_err(|e| (StatusCode::CONFLICT, e))?;
    serde_json::from_value(value)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_GATEWAY, format!("the desktop app answered oddly: {e}")))
}

pub(crate) async fn snapshot_route(Json(r): Json<PreviewReq>) -> Result<Json<PreviewSnapshot>, (StatusCode, String)> {
    answer(drive(&r.stream_id, r.view_id.as_deref(), "snapshot", json!({})).await)
}

pub(crate) async fn navigate_route(Json(r): Json<PreviewNavigateReq>) -> Result<Json<PreviewPage>, (StatusCode, String)> {
    answer(drive(&r.stream_id, r.view_id.as_deref(), "navigate", json!({"url": r.url})).await)
}

pub(crate) async fn click_route(Json(r): Json<PreviewClickReq>) -> Result<Json<PreviewPage>, (StatusCode, String)> {
    answer(drive(&r.stream_id, r.view_id.as_deref(), "click", json!({"selector": r.selector})).await)
}

pub(crate) async fn eval_route(Json(r): Json<PreviewEvalReq>) -> Result<Json<PreviewValue>, (StatusCode, String)> {
    answer(drive(&r.stream_id, r.view_id.as_deref(), "eval", json!({"expression": r.expression})).await)
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
pub(crate) struct ScreenshotReq {
    /// The conversation asking, where the capture is noted: an agent's own,
    /// which is also the default.
    #[serde(default)]
    pub(crate) stream_id: String,
}

#[derive(schemars::JsonSchema, Debug, Serialize, Deserialize)]
pub(crate) struct Screenshot {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// The PNG, base64. An agent's tool result carries only the size: its
    /// model gets the image itself beside the result.
    pub(crate) png: String,
}

/// `POST /desktop/screenshot`: the app window as the operator sees it, its
/// own page only; a Code or Preview view is a separate view and never in it.
/// Each capture is noted in the asking conversation, without the pixels.
pub(crate) async fn screenshot_route(
    State(st): State<AppState>,
    Json(r): Json<ScreenshotReq>,
) -> Result<Json<Screenshot>, (StatusCode, String)> {
    if r.stream_id.trim().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "which conversation is asking? stream_id is empty".to_string()));
    }
    let tx = link().lock().unwrap().as_ref().map(|l| l.tx.clone()).ok_or((
        StatusCode::CONFLICT,
        "Bakery's desktop app isn't connected, so there is no window to capture: open the Bakery app".to_string(),
    ))?;
    let Json(shot): Json<Screenshot> =
        answer(relay(tx, json!({"op": "screenshot", "view_id": "", "stream_id": r.stream_id})).await)?;
    let note = json!({"width": shot.width, "height": shot.height}).to_string();
    let actor = crate::actor::attribute("human");
    crate::actor::spawn_blocking(move || crate::state::append(&st, &r.stream_id, &actor, SCREENSHOT_FAMILY, note, vec![]))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(shot))
}

// ponytail: in memory, the latest per agent; a second screenshot in one
// step replaces the first.
fn shots() -> &'static Mutex<HashMap<String, (String, Vec<u8>)>> {
    static SHOTS: OnceLock<Mutex<HashMap<String, (String, Vec<u8>)>>> = OnceLock::new();
    SHOTS.get_or_init(Default::default)
}

/// The screenshot route's answer as an agent's tool result: the PNG is kept
/// aside for the agent's model (`take_screenshot`), and the text, which
/// Heart records, carries only its size.
pub(crate) fn keep_screenshot(agent_id: &str, turn_id: &str, body: &str) -> Result<String, String> {
    let shot: Screenshot = serde_json::from_str(body).map_err(|e| format!("the desktop app answered oddly: {e}"))?;
    let png = crate::attachments::decode_base64(&shot.png)?;
    shots().lock().unwrap().insert(agent_id.to_string(), (turn_id.to_string(), png));
    Ok(json!({"width": shot.width, "height": shot.height, "image": "attached beside this result"}).to_string())
}

/// The screenshot `agent_id` took (in `turn_id`, when given), once.
pub(crate) fn take_screenshot(agent_id: &str, turn_id: Option<&str>) -> Option<Vec<u8>> {
    let (turn, png) = shots().lock().unwrap().remove(agent_id)?;
    turn_id.is_none_or(|t| t == turn).then_some(png)
}

/// A screenshot for a model that reads tool results as text: an image in a
/// message of its own, sent after every result of the step that took it,
/// or a line saying the model can't take images.
pub(crate) fn screenshot_message(png: Vec<u8>, takes: &[&str]) -> bakery_agent::ChatMsg {
    if !takes.contains(&"image/png") {
        return bakery_agent::ChatMsg::user("[desktop_screenshot's image is not shown: this model doesn't take images.]");
    }
    let image = bakery_agent::NativeAttachment {
        filename: "screenshot.png".to_string(),
        mime_type: "image/png".to_string(),
        data: png,
    };
    bakery_agent::ChatMsg::user_with_attachments("The screenshot desktop_screenshot took of the Bakery window:", vec![image])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::Arc;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    /// The node's routes on a random loopback port, on their own runtime.
    fn serve_for_test(state: AppState) -> std::net::SocketAddr {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tokio::runtime::Runtime::new().unwrap().block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(listener.local_addr().unwrap()).unwrap();
                let _ = axum::serve(listener, crate::api_routes().with_state(state)).await;
            });
        });
        let addr = rx.recv().unwrap();
        crate::testutil::first_connection(addr);
        addr
    }

    fn registered(stream_id: &str) -> bool {
        link().lock().unwrap().as_ref().is_some_and(|l| l.previews.iter().any(|p| p.stream_id == stream_id))
    }

    async fn until(what: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !what() {
            assert!(tokio::time::Instant::now() < deadline, "timed out");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[test]
    fn a_screenshot_reaches_a_model_as_an_image_only_where_it_takes_images_and_only_in_its_turn() {
        let msg = screenshot_message(vec![1, 2], &["image/png", "image/jpeg"]);
        assert_eq!(msg.attachments.iter().map(|a| a.mime_type.as_str()).collect::<Vec<_>>(), ["image/png"]);
        let msg = screenshot_message(vec![1, 2], &[]);
        assert!(msg.attachments.is_empty() && msg.content.contains("doesn't take images"), "{}", msg.content);

        let body = json!({"width": 1, "height": 1, "png": "AQI="}).to_string();
        keep_screenshot("agent-of-an-old-turn", "turn-1", &body).unwrap();
        assert_eq!(take_screenshot("agent-of-an-old-turn", Some("turn-2")), None);
        assert_eq!(take_screenshot("agent-of-an-old-turn", Some("turn-1")), None, "a stale one is dropped");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn agents_drive_only_their_own_conversations_previews_through_the_desktop_link() {
        use crate::agent_control::OperatingMode;
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let addr = serve_for_test(state.clone());
        let call = |stream: &str, tool: &str, args: Value| {
            let (state, stream, tool) = (state.clone(), stream.to_string(), tool.to_string());
            tokio::task::spawn_blocking(move || {
                let caller = crate::api_tools::Caller {
                    stream_id: stream,
                    seat: crate::node::Seat::Baker,
                    turn_id: String::new(),
                    mode: OperatingMode::Auto,
                    wait: None,
                };
                crate::api_tools::call(&state, &caller, &tool, &args)
            })
        };

        let err = call("wt:a", "preview_snapshot", json!({})).await.unwrap().unwrap_err();
        assert!(err.contains("isn't connected"), "{err}");

        // The link needs a ticket.
        assert!(tokio_tungstenite::connect_async(format!("ws://{addr}/desktop/ws?ticket=nope")).await.is_err());
        let ticket = crate::state::mint_ws_ticket_string();
        crate::state::mint_events_ticket(&state, ticket.clone());
        let (mut desktop, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/desktop/ws?ticket={ticket}"))
            .await
            .unwrap();
        let previews = json!({"type": "previews", "previews": [{"view_id": "v1", "stream_id": "wt:a", "url": "http://127.0.0.1:5173/"}]});
        desktop.send(WsMessage::Text(previews.to_string().into())).await.unwrap();
        until(|| registered("wt:a")).await;

        let err = call("wt:b", "preview_snapshot", json!({"stream_id": "wt:a"})).await.unwrap().unwrap_err();
        assert!(err.contains("your own conversation only"), "another conversation's preview is refused: {err}");
        let err = call("wt:b", "preview_snapshot", json!({})).await.unwrap().unwrap_err();
        assert!(err.contains("no preview is open"), "{err}");

        // Its own: the command reaches that view in the app, and its answer
        // comes back as the tool's result.
        let pending = call("wt:a", "preview_snapshot", json!({}));
        let command: Value = match desktop.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
            other => panic!("expected a command, got {other:?}"),
        };
        assert_eq!(
            (command["op"].as_str(), command["view_id"].as_str(), command["stream_id"].as_str()),
            (Some("snapshot"), Some("v1"), Some("wt:a"))
        );
        let value = json!({"url": "http://127.0.0.1:5173/", "title": "App", "text": "Hello"});
        let reply = json!({"type": "result", "id": command["id"], "ok": true, "value": value});
        desktop.send(WsMessage::Text(reply.to_string().into())).await.unwrap();
        let snapshot: Value = serde_json::from_str(&pending.await.unwrap().unwrap()).unwrap();
        assert_eq!(snapshot["text"], "Hello");

        // A command that fails in the page fails the tool.
        let pending = call("wt:a", "preview_click", json!({"selector": "#missing"}));
        let command: Value = match desktop.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
            other => panic!("expected a command, got {other:?}"),
        };
        assert_eq!(command["args"]["selector"], "#missing");
        let reply = json!({"type": "result", "id": command["id"], "ok": false, "error": "no element matches #missing"});
        desktop.send(WsMessage::Text(reply.to_string().into())).await.unwrap();
        let err = pending.await.unwrap().unwrap_err();
        assert!(err.contains("no element matches #missing"), "{err}");

        // A screenshot is of the app window, never a preview. The agent's
        // model gets the PNG beside the result; the result and Heart get
        // only its size and a note.
        assert_eq!(crate::api_tools::risk_of("desktop_screenshot"), Some(crate::api_catalog::Risk::Run));
        // The window shows every conversation: the Baker's to capture.
        let err = call("wt:b", "desktop_screenshot", json!({})).await.unwrap().unwrap_err();
        assert!(err.contains("the Baker's only"), "{err}");
        state.node.lock().unwrap().baker_stream_id = Some("wt:a".to_string());
        let pending = call("wt:a", "desktop_screenshot", json!({}));
        let command: Value = match desktop.next().await.unwrap().unwrap() {
            WsMessage::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
            other => panic!("expected a command, got {other:?}"),
        };
        assert_eq!((command["op"].as_str(), command["view_id"].as_str()), (Some("screenshot"), Some("")));
        let png = bakery_agent::tool_wire::encode_base64(b"\x89PNG pixels");
        let shot = json!({"width": 2, "height": 1, "png": png});
        let reply = json!({"type": "result", "id": command["id"], "ok": true, "value": shot});
        desktop.send(WsMessage::Text(reply.to_string().into())).await.unwrap();
        let body = pending.await.unwrap().unwrap();
        assert!(!body.contains(&png) && body.contains("\"width\":2"), "{body}");
        let agent = crate::node::agent_id("wt:a", crate::node::Seat::Baker);
        assert_eq!(take_screenshot(&agent, Some("")).as_deref(), Some(&b"\x89PNG pixels"[..]));
        assert_eq!(take_screenshot(&agent, None), None, "it goes to the model once");
        let events = state.heart.lock().unwrap().read_stream("wt:a").unwrap();
        let note = events.iter().find(|e| e.family == SCREENSHOT_FAMILY).expect("each capture is noted");
        assert_eq!(note.actor, crate::actor::agent("wt:a", crate::node::Seat::Baker));
        assert!(events.iter().all(|e| !e.payload.contains(&png)), "no pixels in Heart");
        // Even the Baker's capture is noted in its own conversation only.
        state.node.lock().unwrap().baker_stream_id = Some("wt:b".to_string());
        let err = call("wt:b", "desktop_screenshot", json!({"stream_id": "wt:a"})).await.unwrap().unwrap_err();
        assert!(err.contains("own previews only"), "noted only in the caller's own conversation: {err}");
        state.node.lock().unwrap().baker_stream_id = None;

        // The app leaving takes its previews with it.
        desktop.close(None).await.unwrap();
        until(|| link().lock().unwrap().is_none()).await;
        let err = call("wt:a", "preview_snapshot", json!({})).await.unwrap().unwrap_err();
        assert!(err.contains("isn't connected"), "{err}");
    }
}
