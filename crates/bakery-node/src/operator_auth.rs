//! Operator authentication for the loopback HTTP surface.
//!
//! Loopback is not a trust boundary: any local process can dial
//! `127.0.0.1:7717`, and a sandboxed ACP child's Seatbelt profile allows
//! network on purpose (its own provider traffic). Before this layer, that
//! meant a task worker's child could bypass its MCP scope entirely by
//! calling ordinary routes (`/task/create`, `/agent/accept`, `/seat/assign`,
//! …) — the exact bypass `mcp.rs`'s module doc recorded as the known gap.
//!
//! The rule, enforced in one place (a `from_fn_with_state` layer wrapped
//! around the whole router, inside CORS so preflight still works):
//!
//! - `GET /health` is open — it is the liveness probe (Playwright's
//!   `webServer` readiness, `curl` sanity checks) and carries nothing.
//! - `/mcp/tools` + `/mcp/call` pass through: they validate their own
//!   per-session MCP bearer and are the ONLY surface an agent credential
//!   grants. An MCP token presented anywhere else is refused with a
//!   scope-shaped 403, not a generic 401 — the distinction is the proof
//!   that agent-scoped access is enforced server-side.
//! - `/mcp` passes through: it takes only the outside window's read-only
//!   token (`outside_window`), which is refused everywhere else, the same
//!   scope-shaped 403.
//! - `/terminal/ws` and `/events` pass through here and enforce their own
//!   one-time tickets (browser WebSocket upgrades cannot carry an
//!   `Authorization` header); the ticket mints themselves
//!   (`POST /terminal/ticket`, `POST /events/ticket`) sit behind this
//!   layer, so a ticket is always downstream of operator auth.
//! - Everything else requires `Authorization: Bearer <operator token>`.
//!
//! The operator token lives in `node.json` (0600, on the sandbox deny
//! list) or `BAKERY_OPERATOR_TOKEN`; children are spawned `env -i` with an
//! explicit allowlist that never includes it.

use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{header::AUTHORIZATION, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

/// The only path that carries no protected state and must stay probe-able:
/// the liveness check the desktop shell (and Playwright's readiness probe)
/// uses to decide whether a node is already listening. There is no
/// credential-dispensing route at all — the desktop shell reads the
/// credential from `node.json` natively.
fn is_open(path: &str) -> bool {
    path == "/health"
}

/// Paths that enforce their own credential (MCP bearer or one-time WS
/// ticket) — the layer lets them through so that credential can be checked
/// where its context lives.
fn is_self_guarded(path: &str) -> bool {
    matches!(
        path,
        "/mcp/tools" | "/mcp/call" | "/mcp" | "/terminal/ws" | "/events" | "/desktop/ws"
    )
}

/// Constant-time-ish equality via blake3 (its `Hash` PartialEq is
/// constant-time) — never a direct `==` on the secret bytes.
pub(crate) fn token_matches(presented: &str, expected: &str) -> bool {
    !expected.is_empty() && blake3::hash(presented.as_bytes()) == blake3::hash(expected.as_bytes())
}

fn bearer(req: &Request) -> Option<String> {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
}

pub(crate) async fn require_operator(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, (StatusCode, String)> {
    let path = req.uri().path();
    if is_open(path) || is_self_guarded(path) {
        return Ok(next.run(req).await);
    }
    match bearer(&req) {
        Some(t) if token_matches(&t, &st.operator_token) => Ok(next.run(req).await),
        Some(t) if st.mcp_tokens.lock().unwrap().contains_key(&t) => Err((
            StatusCode::FORBIDDEN,
            "agent-scoped token: this credential may only call /mcp/tools and /mcp/call — operator routes require the operator token".to_string(),
        )),
        Some(t) if crate::outside_window::admits(&st, &t) => Err((
            StatusCode::FORBIDDEN,
            "read-only token: this credential may only call POST /mcp, the outside window".to_string(),
        )),
        _ => Err((
            StatusCode::UNAUTHORIZED,
            "missing or invalid operator token — send Authorization: Bearer <operator token> (printed at node startup; BAKERY_OPERATOR_TOKEN overrides)".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::*;
    use bakery_agent::StubModel;
    use std::sync::Arc;

    /// The EXACT production app (`crate::build_app` — every route, the CORS
    /// layer, and this module's auth layer) on an ephemeral loopback port,
    /// on its own OS thread/runtime, so plain blocking tests can hit it.
    fn spawn_full_app_for_test(state: crate::state::AppState) -> std::net::SocketAddr {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("auth test server runtime");
            rt.block_on(async move {
                let app = crate::build_app(state);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind auth test server");
                let addr = listener.local_addr().expect("auth test server local_addr");
                addr_tx.send(addr).expect("send auth test server addr");
                let _ = axum::serve(listener, app).await;
            });
        });
        let addr = addr_rx.recv().expect("receive auth test server addr");
        crate::testutil::first_connection(addr);
        // Readiness barrier: under a full-workspace `cargo test` (hundreds
        // of threads + bakery-exec's sandbox spawns) the fresh server
        // thread can be starved long enough for a single unretried connect
        // to time out — observed as a load-only flake. Retry with short
        // connect timeouts until the server actually answers.
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(2))
            .timeout(std::time::Duration::from_secs(5))
            .build();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        loop {
            match agent.get(&format!("http://{addr}/health")).call() {
                Ok(_) => break,
                Err(ureq::Error::Status(_, _)) => break, // an HTTP answer IS readiness
                Err(_) => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "auth test server never became reachable"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
        addr
    }

    #[test]
    fn health_is_open_and_operator_routes_refuse_without_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let addr = spawn_full_app_for_test(state);

        // Liveness probe stays unauthenticated.
        assert_eq!(
            ureq::get(&format!("http://{addr}/health"))
                .call()
                .unwrap()
                .status(),
            200
        );

        // A read route and a mutating route both refuse with 401 — the
        // Deploy / Restart / Roll back route included.
        for (method, path) in [
            ("GET", "/node"),
            ("GET", "/streams"),
            ("POST", "/task/list"),
            ("GET", "/control/bakery"),
            ("POST", "/control/bakery"),
        ] {
            let err = match method {
                "GET" => ureq::get(&format!("http://{addr}{path}"))
                    .call()
                    .unwrap_err(),
                _ => ureq::post(&format!("http://{addr}{path}"))
                    .send_json(serde_json::json!({}))
                    .unwrap_err(),
            };
            match err {
                ureq::Error::Status(code, resp) => {
                    assert_eq!(code, 401, "{method} {path}");
                    let body = resp.into_string().unwrap();
                    assert!(body.contains("operator token"), "{body}");
                }
                other => panic!("expected 401, got {other:?}"),
            }
        }

        // The operator token opens them.
        let ok = ureq::get(&format!("http://{addr}/node"))
            .set("Authorization", "Bearer test-operator-token")
            .call()
            .unwrap();
        assert_eq!(ok.status(), 200);
    }

    /// The bypass this layer closes: an agent's MCP token must open ONLY
    /// the MCP surface — presented on an ordinary route it is refused with
    /// a scope-shaped 403, not accepted and not a confusing 401.
    #[test]
    fn an_mcp_token_is_scope_refused_on_operator_routes_but_valid_on_its_own_surface() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let baker_home = crate::routes::baker::ensure_permanent_baker(&state, None, None).unwrap();
        let token = crate::mcp::mint_mcp_token(
            &state,
            &baker_home.stream_id,
            crate::node::Seat::Baker,
            crate::mcp::McpScope::BakerOps,
        );
        let addr = spawn_full_app_for_test(state);

        let err = ureq::get(&format!("http://{addr}/node"))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap_err();
        match err {
            ureq::Error::Status(code, resp) => {
                assert_eq!(code, 403);
                let body = resp.into_string().unwrap();
                assert!(body.contains("agent-scoped"), "{body}");
            }
            other => panic!("expected 403, got {other:?}"),
        }

        // The same token IS valid on its own surface, through the same app.
        let tools = ureq::get(&format!("http://{addr}/mcp/tools"))
            .set("Authorization", &format!("Bearer {token}"))
            .call()
            .unwrap();
        assert_eq!(tools.status(), 200);
    }

    fn events_ws_request(
        addr: std::net::SocketAddr,
        ticket: &str,
    ) -> tokio_tungstenite::tungstenite::handshake::client::Request {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://{addr}/events?ticket={ticket}")
            .into_client_request()
            .unwrap();
        req.headers_mut()
            .insert("Origin", "http://localhost:1420".parse().unwrap());
        req
    }

    fn assert_ws_403(err: tokio_tungstenite::tungstenite::Error) {
        match err {
            tokio_tungstenite::tungstenite::Error::Http(resp) => {
                assert_eq!(resp.status(), 403, "expected a 403 WS rejection")
            }
            other => panic!("expected an HTTP-level rejection, got {other:?}"),
        }
    }

    /// `/events` was the unguarded read channel (every stream's events and
    /// activity, to any local process). Now: no ticket → 403; a ticket
    /// minted over the authenticated POST → upgrade; same ticket twice →
    /// 403 (one-time).
    #[tokio::test]
    async fn events_ws_requires_a_one_time_ticket_minted_behind_operator_auth() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let addr = spawn_full_app_for_test(state);

        let err = tokio_tungstenite::connect_async(events_ws_request(addr, "not-a-ticket"))
            .await
            .unwrap_err();
        assert_ws_403(err);

        // Minting the ticket itself requires the operator token.
        let unauth = tokio::task::spawn_blocking(move || {
            ureq::post(&format!("http://{addr}/events/ticket"))
                .send_json(serde_json::json!({}))
                .unwrap_err()
        })
        .await
        .unwrap();
        assert!(matches!(unauth, ureq::Error::Status(401, _)));

        let ticket: String = tokio::task::spawn_blocking(move || {
            let v: serde_json::Value = ureq::post(&format!("http://{addr}/events/ticket"))
                .set("Authorization", "Bearer test-operator-token")
                .send_json(serde_json::json!({}))
                .unwrap()
                .into_json()
                .unwrap();
            v["ticket"].as_str().unwrap().to_string()
        })
        .await
        .unwrap();

        let (mut ws, _resp) = tokio_tungstenite::connect_async(events_ws_request(addr, &ticket))
            .await
            .expect("a fresh ticket must upgrade");
        let _ = futures_util::SinkExt::close(&mut ws).await;

        let err = tokio_tungstenite::connect_async(events_ws_request(addr, &ticket))
            .await
            .unwrap_err();
        assert_ws_403(err);
    }

    #[test]
    fn token_matches_is_exact_and_refuses_empty_expected() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(
            !token_matches("", ""),
            "an empty expected token must match nothing"
        );
        assert!(!token_matches("anything", ""));
    }

    /// The credential-dispensing route is gone: nothing unauthenticated
    /// hands out the operator token any more (the desktop shell reads it
    /// from node.json natively), so the old pairing endpoint must 401 like
    /// any other unknown/authenticated surface rather than answer.
    #[test]
    fn there_is_no_unauthenticated_route_that_dispenses_the_operator_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with(dir.path(), Arc::new(StubModel));
        let addr = spawn_full_app_for_test(state);

        let err = ureq::post(&format!("http://{addr}/operator/pair"))
            .send_json(serde_json::json!({ "ticket": "anything" }))
            .unwrap_err();
        match err {
            ureq::Error::Status(code, resp) => {
                assert!(
                    code == 401 || code == 404,
                    "pairing must not answer, got {code}"
                );
                let body = resp.into_string().unwrap_or_default();
                assert!(
                    !body.contains("test-operator-token"),
                    "no route may return the operator token"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn exemption_lists_are_exactly_the_documented_set() {
        for p in ["/health"] {
            assert!(is_open(p));
        }
        assert!(
            !is_open("/operator/pair"),
            "browser pairing is gone — nothing but /health is open"
        );
        for p in ["/mcp/tools", "/mcp/call", "/terminal/ws", "/events"] {
            assert!(is_self_guarded(p), "{p}");
        }
        for p in [
            "/task/create",
            "/agent/accept",
            "/agent/reject",
            "/agent/turn",
            "/seat/assign",
            "/providers",
            "/env/exec",
            "/env/pty",
            "/terminal/create",
            "/terminal/ticket",
            "/events/ticket",
            "/files/write",
            "/policy/write-mode",
            "/task/signoff",
        ] {
            assert!(
                !is_open(p) && !is_self_guarded(p),
                "{p} must require the operator token"
            );
        }
    }
}
