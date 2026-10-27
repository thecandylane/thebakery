//! The grok CLI door. The room holds a stand-in `auth.json` (the `key` is
//! this thread's minted stand-in, and there is no refresh token). The node
//! reads the operator's grok login only to swap that stand-in, and only on
//! a request to `cli-chat-proxy.grok.com`. An API key in the same file is
//! not the door's credential: HTTP model keys stay on the node.
//!
//! The file shape is the one grok-build writes: `$GROK_HOME/auth.json`
//! (`GROK_AUTH_PATH` overrides), a map of `{issuer}::{client_id}` to a
//! `GrokAuth` object whose `key` is the bearer. The chat proxy base URL is
//! `GROK_CLI_CHAT_PROXY_BASE_URL` and `GROK_PRODUCTION_CLI_CHAT_PROXY_BASE_URL`.
//!
//! cursor-agent's browser login is the macOS keychain (or, when that is
//! unavailable, a file whose schema is not in Cursor's public docs). This
//! module does not read a Cursor login and does not send a Grok token to
//! Cursor. That door is unfinished.

use crate::state::AppState;
use std::path::{Path, PathBuf};

/// Documented seat ids for the grok CLI plug. A plug with another id still
/// matches when its binary is the `grok` executable ([`serves_model`]).
pub(crate) const MODEL_IDS: &[&str] = &["acp:grok", "grok-acp"];

pub(crate) const CHAT_HOST: &str = "cli-chat-proxy.grok.com";
const OIDC_ISSUER: &str = "https://auth.x.ai";
const OIDC_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const API_KEY_SCOPE: &str = "xai::api_key";

/// Why cursor-agent has no row. Kept as a value a test can name.
pub(crate) const CURSOR_DOOR_UNFINISHED: &str = "cursor-agent stores its browser login in the macOS keychain, or in a file whose schema is not in Cursor's public docs. The door is not wired, and the login folder is not copied.";

pub(crate) fn known_model(model_id: &str) -> bool {
    MODEL_IDS.iter().any(|id| model_id.eq_ignore_ascii_case(id))
}

fn binary_name(argv0: &str) -> Option<&str> {
    Path::new(argv0).file_name().and_then(|name| name.to_str())
}

/// The xAI `grok` binary, including `~/.grok/bin/agent` on this Mac. A bare
/// `agent` that is not under `.grok` is not assumed to be grok: that name is
/// also how cursor-agent is installed.
pub(crate) fn is_grok_binary(argv0: &str) -> bool {
    match binary_name(argv0) {
        Some("grok") => true,
        Some("agent") => argv0.contains("/.grok/"),
        _ => false,
    }
}

pub(crate) fn is_cursor_binary(argv0: &str, plug_id: &str) -> bool {
    match binary_name(argv0) {
        Some("cursor-agent") => true,
        Some("agent") if !argv0.contains("/.grok/") && plug_id.to_ascii_lowercase().contains("cursor") => true,
        _ => false,
    }
}

/// Whether this seat's model is the grok CLI. Plug lookup takes the node
/// lock only with `try_lock`, so it never waits while the Heart lock is held.
pub(crate) fn serves_model(state: &AppState, model_id: &str) -> bool {
    if known_model(model_id) {
        return true;
    }
    let Ok(node) = state.node.try_lock() else {
        return false;
    };
    node.acp_plugs.iter().any(|plug| plug.id == model_id && plug.argv.first().is_some_and(|bin| is_grok_binary(bin)))
}

fn auth_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("BAKERY_GROK_AUTH_FILE") {
        return Some(PathBuf::from(path));
    }
    // Tests never open the operator's real login. The node binary does.
    #[cfg(test)]
    {
        return None;
    }
    #[cfg(not(test))]
    {
        if let Some(path) = std::env::var_os("GROK_AUTH_PATH") {
            return Some(PathBuf::from(path));
        }
        let home = std::env::var_os("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".grok")))?;
        Some(home.join("auth.json"))
    }
}

/// The access token (`key`) of an OIDC or external login. The API-key scope
/// and every refresh token are left unread by the caller: this returns only
/// the bearer the door swaps in.
pub(crate) fn access_token_from_auth_json(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let mut fallback = None;
    for (scope, entry) in object {
        if scope == API_KEY_SCOPE {
            continue;
        }
        let mode = entry.get("auth_mode").and_then(|mode| mode.as_str()).unwrap_or("");
        if mode != "oidc" && mode != "external" {
            continue;
        }
        let key = entry.get("key").and_then(|key| key.as_str()).filter(|key| !key.is_empty())?;
        if scope.starts_with("https://auth.x.ai::") || entry.get("oidc_issuer").and_then(|v| v.as_str()) == Some(OIDC_ISSUER) {
            return Some(key.to_string());
        }
        if fallback.is_none() {
            fallback = Some(key.to_string());
        }
    }
    fallback
}

pub(crate) fn access_token(_state: &AppState) -> Option<String> {
    let path = auth_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    access_token_from_auth_json(&text)
}

/// The stand-in file the CLI reads. `stand_in` is the minted value. No
/// refresh token is written.
pub(crate) fn write_stand_in(dir: &Path, stand_in: &str) -> Result<PathBuf, String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(dir).map_err(|e| format!("grok stand-in dir: {e}"))?;
    let path = dir.join("auth.json");
    let scope = format!("{OIDC_ISSUER}::{OIDC_CLIENT_ID}");
    let body = serde_json::json!({
        scope: {
            "key": stand_in,
            "auth_mode": "oidc",
            "create_time": "2026-01-01T00:00:00Z",
            "user_id": "bakery",
            "expires_at": "2099-01-01T00:00:00Z",
            "oidc_issuer": OIDC_ISSUER,
            "oidc_client_id": OIDC_CLIENT_ID,
        }
    });
    let bytes = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("grok stand-in: {e}"))?;
    file.write_all(&bytes).map_err(|e| format!("grok stand-in: {e}"))?;
    Ok(path)
}

pub(crate) fn seat_dir(state: &AppState, stream_id: &str, seat: &str) -> PathBuf {
    state.base.join("cache").join("grok-cli").join("seat-config").join(stream_id).join(seat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oidc_login_yields_its_access_token_and_not_the_api_key_or_refresh_token() {
        let text = r#"{
            "https://auth.x.ai::b1a00492-073a-47ea-816f-4c329264a828": {
                "key": "grok-fixture-access",
                "auth_mode": "oidc",
                "refresh_token": "grok-fixture-refresh",
                "create_time": "2026-01-01T00:00:00Z",
                "user_id": "u"
            },
            "xai::api_key": {
                "key": "grok-fixture-api-key",
                "auth_mode": "api_key",
                "create_time": "2026-01-01T00:00:00Z",
                "user_id": "u"
            }
        }"#;
        assert_eq!(access_token_from_auth_json(text).as_deref(), Some("grok-fixture-access"));
    }

    #[test]
    fn the_stand_in_file_holds_the_minted_value_and_no_refresh_token() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_stand_in(dir.path(), "bakery-stand-in-grok-cli-abc").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("bakery-stand-in-grok-cli-abc"));
        assert!(!text.contains("refresh_token"));
        assert!(!text.contains("grok-fixture-access"));
        assert_eq!(access_token_from_auth_json(&text).as_deref(), Some("bakery-stand-in-grok-cli-abc"));
    }

    #[test]
    fn grok_and_cursor_binaries_are_not_the_same_door() {
        assert!(is_grok_binary("grok"));
        assert!(is_grok_binary("/Users/operator/.grok/bin/agent"));
        assert!(!is_grok_binary("/usr/local/bin/agent"));
        assert!(is_cursor_binary("cursor-agent", "acp:cursor"));
        assert!(is_cursor_binary("/opt/cursor/agent", "acp:cursor"));
        assert!(!is_cursor_binary("/Users/operator/.grok/bin/agent", "acp:grok"));
        assert!(!is_cursor_binary("agent", "acp:grok"));
        assert!(CURSOR_DOOR_UNFINISHED.contains("not wired"));
    }

    #[test]
    fn a_test_build_does_not_read_the_operator_grok_login() {
        let previous = std::env::var_os("BAKERY_GROK_AUTH_FILE");
        std::env::remove_var("BAKERY_GROK_AUTH_FILE");
        let state = crate::testutil::state_with(tempfile::tempdir().unwrap().path(), std::sync::Arc::new(bakery_agent::StubModel));
        assert!(access_token(&state).is_none());
        if let Some(previous) = previous {
            std::env::set_var("BAKERY_GROK_AUTH_FILE", previous);
        }
    }
}
