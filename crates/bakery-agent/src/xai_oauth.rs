//! xAI Grok OAuth — RFC 8628 device flow (ported from oh-my-pi xai-oauth).
//!
//! oh-my-pi: MIT License, Copyright (c) 2025 Mario Zechner, Copyright (c)
//! 2025-2026 Can Bölük, Copyright (c) 2026 Stencil Labs, Inc. Full text in
//! THIRD_PARTY_NOTICES.md.
//!
//! Uses pure-sync `ureq` so we never nest a Tokio runtime inside bakery-node.

use crate::secrets::{self, OAuthTokens};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
const DEFAULT_TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";
const USERINFO_URL: &str = "https://auth.x.ai/oauth2/userinfo";

#[derive(schemars::JsonSchema, Debug, Clone, serde::Serialize)]
pub struct DeviceStart {
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub interval_seconds: u64,
    pub expires_in_seconds: u64,
    pub device_code: String,
}

#[derive(schemars::JsonSchema, Debug, Clone, serde::Serialize)]
pub struct DevicePollResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

static PENDING: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn discover_token_endpoint() -> String {
    let Ok(resp) = ureq::get(DISCOVERY_URL)
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call()
    else {
        return DEFAULT_TOKEN_URL.to_string();
    };
    let Ok(v) = resp.into_json::<Value>() else {
        return DEFAULT_TOKEN_URL.to_string();
    };
    v.get("token_endpoint")
        .and_then(|t| t.as_str())
        .filter(|u| u.starts_with("https://") && (u.contains("x.ai") || u.contains(".x.ai")))
        .unwrap_or(DEFAULT_TOKEN_URL)
        .to_string()
}

/// Start device authorization. Client shows user_code + opens verification_uri_complete.
pub fn device_start() -> Result<DeviceStart, String> {
    let body = format!("client_id={CLIENT_ID}&scope={}", urlencoding_scope(SCOPE));
    let resp = ureq::post(DEVICE_CODE_URL)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send_string(&body)
        .map_err(|e| format!("xAI device-code: {e}"))?;
    let status = resp.status();
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "xAI device-code {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let device_code = str_field(&v, "device_code")?;
    let user_code = str_field(&v, "user_code")?;
    let verification_uri = str_field(&v, "verification_uri")?;
    let verification_uri_complete = str_field(&v, "verification_uri_complete")
        .unwrap_or_else(|_| format!("{verification_uri}?user_code={user_code}"));
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(900);
    let interval = v.get("interval").and_then(|x| x.as_u64()).unwrap_or(5);

    {
        let mut g = PENDING.lock().unwrap();
        let map = g.get_or_insert_with(HashMap::new);
        map.insert(device_code.clone(), Instant::now());
    }

    Ok(DeviceStart {
        user_code,
        verification_uri,
        verification_uri_complete,
        interval_seconds: interval,
        expires_in_seconds: expires_in,
        device_code,
    })
}

/// Poll once for completion. On success, tokens are saved to secrets store.
pub fn device_poll(device_code: &str) -> Result<DevicePollResult, String> {
    let device_code = device_code.trim();
    if device_code.is_empty() {
        return Err("device_code required".into());
    }
    let token_url = discover_token_endpoint();
    let body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&client_id={CLIENT_ID}&device_code={}",
        form_encode(device_code)
    );
    let resp = ureq::post(&token_url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send_string(&body);

    let (status, text) = match resp {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(code, r)) => (code, r.into_string().unwrap_or_default()),
        Err(e) => return Err(format!("xAI poll: {e}")),
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);

    if (200..300).contains(&status) {
        let tokens = parse_token_json(&v, None)?;
        let email = fetch_email(&tokens.access).or(tokens.email.clone());
        let mut t = tokens;
        t.email = email.clone();
        secrets::set_xai_oauth(t)?;
        if let Ok(mut g) = PENDING.lock() {
            if let Some(m) = g.as_mut() {
                m.remove(device_code);
            }
        }
        return Ok(DevicePollResult {
            status: "complete".into(),
            error: None,
            email,
        });
    }

    let err = v.get("error").and_then(|e| e.as_str()).unwrap_or("");
    match err {
        "authorization_pending" => Ok(DevicePollResult {
            status: "pending".into(),
            error: None,
            email: None,
        }),
        "slow_down" => Ok(DevicePollResult {
            status: "slow_down".into(),
            error: None,
            email: None,
        }),
        other => Ok(DevicePollResult {
            status: "error".into(),
            error: Some(
                v.get("error_description")
                    .and_then(|d| d.as_str())
                    .unwrap_or(other)
                    .to_string(),
            ),
            email: None,
        }),
    }
}

/// Ensure a valid access token (refresh if needed). Returns Bearer token.
pub fn access_token() -> Result<String, String> {
    let mut s = secrets::load();
    let Some(mut oauth) = s.xai_oauth.clone() else {
        return Err("no xAI OAuth tokens".into());
    };
    if is_fresh(oauth.expires) && !oauth.access.is_empty() {
        return Ok(oauth.access);
    }
    if oauth.refresh.is_empty() {
        return Err("xAI OAuth expired and no refresh token".into());
    }
    oauth = refresh(&oauth.refresh)?;
    s.xai_oauth = Some(oauth.clone());
    secrets::save(&s)?;
    Ok(oauth.access)
}

pub fn refresh(refresh_token: &str) -> Result<OAuthTokens, String> {
    let token_url = discover_token_endpoint();
    let body = format!(
        "grant_type=refresh_token&client_id={CLIENT_ID}&refresh_token={}",
        form_encode(refresh_token)
    );
    let resp = ureq::post(&token_url)
        .set("Content-Type", "application/x-www-form-urlencoded")
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(30))
        .send_string(&body)
        .map_err(|e| format!("xAI refresh: {e}"))?;
    let status = resp.status();
    let text = resp.into_string().map_err(|e| e.to_string())?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "xAI refresh {status}: {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    parse_token_json(&v, Some(refresh_token))
}

fn parse_token_json(v: &Value, refresh_fallback: Option<&str>) -> Result<OAuthTokens, String> {
    let access = v
        .get("access_token")
        .and_then(|a| a.as_str())
        .ok_or("missing access_token")?
        .to_string();
    let refresh = v
        .get("refresh_token")
        .and_then(|a| a.as_str())
        .map(|s| s.to_string())
        .or_else(|| refresh_fallback.map(|s| s.to_string()))
        .ok_or("missing refresh_token")?;
    let expires_in = v.get("expires_in").and_then(|e| e.as_i64()).unwrap_or(3600);
    Ok(OAuthTokens {
        access,
        refresh,
        expires: now_ms() + expires_in * 1000 - 60_000,
        email: None,
        project: None,
    })
}

fn is_fresh(expires: i64) -> bool {
    if expires <= 0 {
        return true;
    }
    expires > now_ms()
}

fn fetch_email(access: &str) -> Option<String> {
    let resp = ureq::get(USERINFO_URL)
        .set("Authorization", &format!("Bearer {access}"))
        .set("Accept", "application/json")
        .timeout(Duration::from_secs(15))
        .call()
        .ok()?;
    let v: Value = resp.into_json().ok()?;
    v.get("email")
        .and_then(|e| e.as_str())
        .map(|s| s.to_string())
}

fn str_field(v: &Value, k: &str) -> Result<String, String> {
    v.get(k)
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing {k}"))
}

fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn urlencoding_scope(s: &str) -> String {
    form_encode(s).replace("%20", "+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encode_safe() {
        assert_eq!(form_encode("abc"), "abc");
        assert!(form_encode("a b").contains('%'));
    }

    #[test]
    fn parse_token_json_ok() {
        let v = serde_json::json!({
            "access_token": "at",
            "refresh_token": "rt",
            "expires_in": 3600
        });
        let t = parse_token_json(&v, None).unwrap();
        assert_eq!(t.access, "at");
        assert_eq!(t.refresh, "rt");
        assert!(t.expires > now_ms());
    }
}
