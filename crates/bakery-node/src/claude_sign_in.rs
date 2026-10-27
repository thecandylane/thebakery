//! The operator's own Claude subscription sign-in: the node's, and never a
//! room's.
//!
//! Claude Code keeps this sign-in as `claudeAiOauth` in `.credentials.json`:
//! an access token, a refresh token, an expiry. Here it lives in the node's
//! vault, out of every room's reach, and the node refreshes it itself. At a
//! thread's door the swap replaces the stand-in a Claude seat holds with the
//! real access token, so a room never holds the access token, the refresh
//! token, or anything that can mint one.
//!
//! Only a `claude-native` thread is served (see `crate::swap`), and the
//! stand-in is minted per thread, so holding it is the whole entitlement.
//!
//! Operator decision, 2026-10-11 (see CLAUDE.md): the node reading its own
//! sign-in at run time is how the swap works. That is not an agent reading
//! it. Agents and dev sessions never open the vault.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Claude Code's own OAuth client, whose sign-in this is.
const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// Where that client's refresh token is exchanged for an access token.
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// Refresh this long before the access token's own expiry.
const EARLY: u64 = 300;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Claude Code's sign-in, as Claude Code writes it. The field names are
/// Claude Code's, not ours.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct OAuth {
    #[serde(rename = "accessToken", default)]
    pub(crate) access: String,
    #[serde(rename = "refreshToken", default)]
    pub(crate) refresh: String,
    /// Epoch milliseconds, as the plaintext file stores it.
    #[serde(rename = "expiresAt", default)]
    pub(crate) expires_at: i64,
    #[serde(rename = "subscriptionType", default, skip_serializing_if = "Option::is_none")]
    pub(crate) subscription: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) scopes: Vec<String>,
    /// Every other field Claude Code keeps in the record — the plan's
    /// `rateLimitTier`, and any sibling plan or subscription field a newer
    /// Claude Code reads — kept verbatim so the vault round-trips them and
    /// the stand-in carries them, and a seat sees the operator's real plan
    /// (its explore-agent count, its usage thresholds). The credential's own
    /// fields are named above and consumed first, so no access or refresh
    /// token ever lands here.
    #[serde(flatten)]
    pub(crate) extra: serde_json::Map<String, serde_json::Value>,
}

impl OAuth {
    fn usable(&self) -> bool {
        !self.access.is_empty() || !self.refresh.is_empty()
    }

    /// Whether the access token is far enough from its expiry to use as is.
    fn fresh(&self) -> bool {
        self.expires_at > now_ms() as i64 + (EARLY as i64) * 1000
    }
}

#[derive(Deserialize, Serialize)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude: OAuth,
}

/// The sign-in file the login button already writes:
/// `cache/claude-native/config/.credentials.json`. One file. The node reads
/// it and refresh overwrites that same file. A seat is never granted it.
pub(crate) fn vault_dir(state: &AppState) -> PathBuf {
    state
        .base
        .join("cache")
        .join("claude-native")
        .join("config")
}

pub(crate) fn vault_file(state: &AppState) -> PathBuf {
    vault_dir(state).join(".credentials.json")
}

fn read_oauth(path: &std::path::Path) -> Option<OAuth> {
    let text = std::fs::read_to_string(path).ok()?;
    let record: Credentials = serde_json::from_str(&text).ok()?;
    record.claude.usable().then_some(record.claude)
}

/// What the node knows of the operator's sign-in, or `None` when they are
/// signed out.
pub(crate) fn stored(state: &AppState) -> Option<OAuth> {
    read_oauth(&vault_file(state))
}

/// Whether the operator has signed the Bakery in to Claude.
#[allow(dead_code)] // status and the seat gate read `stored`; this is the boolean form
pub(crate) fn signed_in(state: &AppState) -> bool {
    stored(state).is_some()
}

/// Persist the sign-in (a login, or a rotation the refresh brought back).
/// 0600, written whole and renamed, so a reader never sees half a record.
pub(crate) fn write_vault(state: &AppState, oauth: &OAuth) -> Result<(), String> {
    let dir = vault_dir(state);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let file = vault_file(state);
    let mut record = std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .filter(|value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    record["claudeAiOauth"] = serde_json::to_value(oauth)
        .map_err(|e| format!("the Claude sign-in could not be written: {e}"))?;
    let text = serde_json::to_vec(&record)
        .map_err(|e| format!("the Claude sign-in could not be written: {e}"))?;
    let tmp = dir.join(".credentials.json.partial");
    let write = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&text)?;
        f.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the Claude sign-in could not be written: {e}"));
    }
    if let Err(e) = std::fs::rename(&tmp, &file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("the Claude sign-in could not be written: {e}"));
    }
    let _ = std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600));
    // A new record means the cached access token is stale (a rotation) or
    // revoked (a fresh login): forget it so the door re-reads this file.
    clear_cache(&file);
    Ok(())
}

/// Google-style test override for the token endpoint.
#[cfg(test)]
fn test_token_url() -> &'static Mutex<Option<String>> {
    static URL: Mutex<Option<String>> = Mutex::new(None);
    &URL
}

fn token_url() -> String {
    #[cfg(test)]
    if let Some(url) = test_token_url().lock().unwrap().clone() {
        return url;
    }
    TOKEN_URL.to_string()
}

/// Exchange the sign-in's refresh token for an access token. Claude answers
/// with a rotated refresh token; when it does not, the old one is kept.
fn refresh(state: &AppState, oauth: &OAuth) -> Option<OAuth> {
    if oauth.refresh.is_empty() {
        return None;
    }
    let answer: serde_json::Value = ureq::post(&token_url())
        .timeout(Duration::from_secs(30))
        .send_json(serde_json::json!({
            "grant_type": "refresh_token",
            "client_id": CLIENT_ID,
            "refresh_token": oauth.refresh,
        }))
        .ok()?
        .into_json()
        .ok()?;
    let access = answer["access_token"].as_str()?.to_string();
    let rotated = answer["refresh_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .unwrap_or(&oauth.refresh)
        .to_string();
    let expires_in = answer["expires_in"].as_u64().unwrap_or(3600);
    let fresh = OAuth {
        access,
        refresh: rotated,
        expires_at: now_ms() as i64 + (expires_in as i64) * 1000,
        subscription: answer["subscriptionType"]
            .as_str()
            .map(str::to_string)
            .or_else(|| oauth.subscription.clone()),
        scopes: oauth.scopes.clone(),
        // The plan fields (rateLimitTier and the like) outlive a rotation:
        // the token endpoint only rotates tokens, so the stored plan stays.
        extra: oauth.extra.clone(),
    };
    // A rotated refresh token has to land before it is used again, or the
    // next refresh is rejected and the operator is signed out.
    let _ = write_vault(state, &fresh);
    Some(fresh)
}

/// A minted access token, kept until the token's OWN wall-clock expiry
/// (`expires_at`, epoch ms) — never a monotonic `Instant`, which stops while
/// the Mac sleeps and so would keep serving a token that already expired
/// during the sleep. Keyed by the vault file, each entry also holds that
/// file's mtime, so a vault rewritten under it (a refresh rotation, the
/// login Keychain copy, or the operator's own `claude login` in the sign-in
/// terminal) is a miss, never served stale.
type Cached = (String, i64, Option<SystemTime>);
fn cache() -> &'static Mutex<HashMap<PathBuf, Cached>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Cached>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// One refresh at a time per vault: two callers near expiry must not both
/// spend the same rotating refresh token, or the second's `invalid_grant`
/// signs the operator out.
fn refresh_gate(key: &Path) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    GATES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key.to_path_buf())
        .or_default()
        .clone()
}

fn vault_mtime(key: &Path) -> Option<SystemTime> {
    std::fs::metadata(key).and_then(|meta| meta.modified()).ok()
}

/// Forget any cached token for the vault at `key`. Called wherever the vault
/// file is written — a refresh rotation, the login Keychain copy — so the
/// door never serves a token a new sign-in revoked.
pub(crate) fn clear_cache(key: &Path) {
    cache().lock().unwrap().remove(key);
}

/// The cached token for `key`, only while it is fresh by wall clock and the
/// vault file has not been rewritten under it (its mtime still matches).
fn fresh_cached(key: &Path, mtime: Option<SystemTime>) -> Option<String> {
    let cache = cache().lock().unwrap();
    let (token, expires_at, cached_mtime) = cache.get(key)?;
    let fresh = *expires_at > now_ms() as i64 + (EARLY as i64) * 1000;
    (fresh && *cached_mtime == mtime).then(|| token.clone())
}

/// The real access token, refreshed when it is close to expiring. Read only
/// by the node: the swap at a thread's door, and the status probe.
pub(crate) fn access_token(state: &AppState) -> Option<String> {
    let key = vault_file(state);
    if let Some(token) = fresh_cached(&key, vault_mtime(&key)) {
        return Some(token);
    }
    // A miss, or near expiry: refresh, but only one caller at a time for this
    // vault, so a rotating refresh token is spent once.
    let gate = refresh_gate(&key);
    let _one = gate.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // Another caller may have refreshed while this one waited for the gate.
    if let Some(token) = fresh_cached(&key, vault_mtime(&key)) {
        return Some(token);
    }
    let stored = stored(state)?;
    let sign_in = if stored.fresh() {
        stored
    } else {
        refresh(state, &stored)?
    };
    // `refresh` rewrote the vault (new mtime) and cleared this entry; read the
    // mtime now, so the cache matches the file and the next read is a hit.
    let mtime = vault_mtime(&key);
    cache()
        .lock()
        .unwrap()
        .insert(key, (sign_in.access.clone(), sign_in.expires_at, mtime));
    Some(sign_in.access)
}

/// This thread's stand-in, minted by the door for whichever subscription
/// this seat model is. The value is not Claude-specific; the name comes
/// from the subscription table.
pub(crate) fn stand_in(state: &AppState, stream_id: &str) -> String {
    crate::swap::minted_for_model(state, crate::claude_native::ACCOUNT_ID, stream_id).unwrap_or_default()
}

/// The plan fields a seat's stand-in may carry from the record, beside the
/// named `subscriptionType` and `scopes`. An allowlist, not every unknown
/// field: a room can read the stand-in, so a field Claude Code may one day
/// add that holds a secret (an id token, a session or org key) must never
/// ride along by default. A new plan field reaches seats only when it is
/// added here on purpose.
const SEAT_PLAN_FIELDS: &[&str] = &["rateLimitTier"];

/// What a Claude seat holds in place of the sign-in: a well-formed
/// `claudeAiOauth` record whose access token is this thread's stand-in. The
/// engine reads it as an ordinary subscription sign-in and the door makes the
/// token real on the way out. No refresh token, and an expiry far enough out
/// that the engine never tries to refresh one. The plan the operator pays for
/// carries through — its `subscriptionType`, `scopes` and the
/// [`SEAT_PLAN_FIELDS`] (today `rateLimitTier`) — so a seat's Claude Code
/// runs on the real plan (its explore-agent count, its usage thresholds).
/// Still nothing the operator holds: the stand-in replaces the access token,
/// carries no refresh token and no other field of the record, so a room
/// holding it holds no credential.
pub(crate) fn stand_in_credential(state: &AppState, stream_id: &str) -> Option<String> {
    let real = stored(state)?;
    // Build the seat's record from nothing, and copy only the allowlisted
    // plan fields across — never the whole `extra`, which may one day hold a
    // secret a room must not see.
    let mut oauth = serde_json::Map::new();
    for field in SEAT_PLAN_FIELDS {
        if let Some(value) = real.extra.get(*field) {
            oauth.insert((*field).to_string(), value.clone());
        }
    }
    oauth.insert("accessToken".to_string(), serde_json::json!(stand_in(state, stream_id)));
    oauth.insert("expiresAt".to_string(), serde_json::json!(now_ms() + 365 * 24 * 60 * 60 * 1000));
    oauth.insert("subscriptionType".to_string(), serde_json::json!(real.subscription));
    oauth.insert("scopes".to_string(), serde_json::json!(real.scopes));
    let blob = serde_json::json!({ "claudeAiOauth": oauth });
    Some(blob.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn state_at(dir: &std::path::Path) -> AppState {
        crate::testutil::state_with(dir, Arc::new(bakery_agent::StubModel))
    }

    /// `test_token_url` is process-wide, so the tests that point it at a
    /// stand-in token server run one at a time, not racing each other.
    fn token_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    const SIGNED_IN: &str = r#"{"claudeAiOauth":{"accessToken":"real-access","refreshToken":"real-refresh","expiresAt":99999999999999,"subscriptionType":"max","scopes":["user:inference"],"rateLimitTier":"max_20x","planMeta":{"explore":3}}}"#;

    #[test]
    fn the_vault_is_the_operators_record_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        assert!(!signed_in(&state));
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(vault_file(&state), SIGNED_IN).unwrap();
        assert!(
            vault_file(&state).ends_with("cache/claude-native/config/.credentials.json"),
            "the sign-in stays the file the login button already writes"
        );
        let stored = stored(&state).expect("signed in");
        assert_eq!(stored.access, "real-access");
        assert_eq!(stored.refresh, "real-refresh");
        assert_eq!(stored.subscription.as_deref(), Some("max"));
        assert_eq!(
            stored.extra.get("rateLimitTier").and_then(|v| v.as_str()),
            Some("max_20x"),
            "the plan's tier is kept in the record"
        );
        assert!(stored.extra.contains_key("planMeta"), "any sibling plan field is kept too");
        assert!(stored.fresh(), "an expiry far out is used as is");
        // The file stays the operator's: reading never rewrites it.
        assert_eq!(std::fs::read_to_string(vault_file(&state)).unwrap(), SIGNED_IN);
    }

    #[test]
    fn the_access_token_is_refreshed_when_it_is_near_expiry_and_the_rotation_is_kept() {
        let _serial = token_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(
            vault_file(&state),
            r#"{"claudeAiOauth":{"accessToken":"stale","refreshToken":"old-refresh","expiresAt":1,"rateLimitTier":"max_20x"}}"#,
        )
        .unwrap();

        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let site_at = site.local_addr().unwrap();
        *test_token_url().lock().unwrap() = Some(format!("http://127.0.0.1:{}/token", site_at.port()));
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n")
                    && std::io::Read::read(&mut conn, &mut byte).unwrap_or(0) == 1
                {
                    head.push(byte[0]);
                }
                if head.is_empty() {
                    // The first-connection wait, which asks nothing.
                    continue;
                }
                let header = String::from_utf8_lossy(&head);
                let length = header.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                let mut body_in = vec![0u8; length.unwrap_or(0)];
                if !body_in.is_empty() {
                    let _ = std::io::Read::read_exact(&mut conn, &mut body_in);
                }
                let _ = seen_tx.send(format!("{header}{}", String::from_utf8_lossy(&body_in)));
                let body = r#"{"access_token":"new-access","refresh_token":"rotated","expires_in":3600}"#;
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(answer.as_bytes());
            }
        });
        crate::testutil::first_connection(site_at);
        assert_eq!(access_token(&state).as_deref(), Some("new-access"));
        let asked = seen.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        assert!(asked.contains("grant_type"), "{asked}");
        assert!(asked.contains(CLIENT_ID), "{asked}");
        assert!(asked.contains("old-refresh"), "{asked}");
        let written = std::fs::read_to_string(vault_file(&state)).unwrap();
        assert!(written.contains("new-access") && written.contains("rotated"), "{written}");
        assert!(!written.contains("stale"), "the rotated record replaces the stale one");
        assert!(written.contains("\"rateLimitTier\":\"max_20x\""), "the plan's tier survives a rotation: {written}");
        // The minted token is cached, so a second read does not refresh again.
        assert_eq!(access_token(&state).as_deref(), Some("new-access"));
    }

    /// The cache is timed by the token's own wall clock, not a monotonic
    /// Instant that stops while the Mac sleeps: an entry whose expiry has
    /// passed is not served, even though it was just inserted (never "timed
    /// out"). The vault's own token is still fresh, so the read falls through
    /// to it with no refresh.
    #[test]
    fn an_expired_cache_entry_is_not_served() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(vault_file(&state), SIGNED_IN).unwrap(); // real-access, far-future expiry
        let key = vault_file(&state);
        // Prime the cache as the old Instant scheme would have: an entry with
        // a matching mtime that "hasn't timed out", but whose token expired
        // (its wall-clock expiry is now, inside the EARLY window).
        cache().lock().unwrap().insert(key.clone(), ("stale-cached".to_string(), now_ms() as i64, vault_mtime(&key)));
        assert_eq!(access_token(&state).as_deref(), Some("real-access"), "the expired entry is not served; the vault's fresh token is");
    }

    /// A vault rewritten under a cached entry is served fresh, never the old
    /// token: the mtime no longer matches (and write_vault clears the entry),
    /// so the door re-reads the file.
    #[test]
    fn a_vault_rewritten_after_caching_is_served_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(vault_file(&state), SIGNED_IN).unwrap();
        assert_eq!(access_token(&state).as_deref(), Some("real-access"), "cached on first read");

        // A fresh login rewrites the vault out of band (its mtime changes).
        let relogged = r#"{"claudeAiOauth":{"accessToken":"real-access-2","refreshToken":"r2","expiresAt":99999999999999,"subscriptionType":"max","scopes":["user:inference"]}}"#;
        // Make sure the mtime differs even on a coarse clock.
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(vault_file(&state), relogged).unwrap();
        assert_eq!(access_token(&state).as_deref(), Some("real-access-2"), "the rewritten token is served, not the cached one");

        // And write_vault (a rotation) clears the entry as well.
        let rotated = OAuth { access: "real-access-3".into(), refresh: "r3".into(), expires_at: 99999999999999, ..Default::default() };
        write_vault(&state, &rotated).unwrap();
        assert_eq!(access_token(&state).as_deref(), Some("real-access-3"), "after write_vault the door re-reads");
    }

    /// Two callers near expiry make exactly one refresh request: the gate
    /// serializes them, and the one that waits finds the cache fresh and does
    /// not spend the rotating refresh token a second time (which would
    /// invalid_grant and sign the operator out).
    #[test]
    fn concurrent_callers_near_expiry_make_one_refresh() {
        use std::io::{Read, Write};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _serial = token_test_lock();
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(
            vault_file(&state),
            r#"{"claudeAiOauth":{"accessToken":"stale","refreshToken":"old-refresh","expiresAt":1}}"#,
        )
        .unwrap();

        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let site_at = site.local_addr().unwrap();
        *test_token_url().lock().unwrap() = Some(format!("http://127.0.0.1:{}/token", site_at.port()));
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_server = hits.clone();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                if head.is_empty() {
                    continue; // the first-connection warm-up asks nothing
                }
                let header = String::from_utf8_lossy(&head);
                let length = header.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
                });
                let mut body_in = vec![0u8; length.unwrap_or(0)];
                if !body_in.is_empty() {
                    let _ = conn.read_exact(&mut body_in);
                }
                hits_server.fetch_add(1, Ordering::SeqCst);
                // A slow reply, so the other callers pile up on the gate while
                // the first one is still refreshing.
                std::thread::sleep(std::time::Duration::from_millis(300));
                let body = r#"{"access_token":"new-access","refresh_token":"rotated","expires_in":3600}"#;
                let answer = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = conn.write_all(answer.as_bytes());
            }
        });
        crate::testutil::first_connection(site_at);

        let got: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..6)
                .map(|_| {
                    let state = state.clone();
                    scope.spawn(move || access_token(&state).unwrap_or_default())
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(got.iter().all(|t| t == "new-access"), "every caller got the refreshed token: {got:?}");
        assert_eq!(hits.load(Ordering::SeqCst), 1, "exactly one refresh request, not one per caller");
    }

    #[test]
    fn a_stand_in_is_per_thread_and_never_the_real_token() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        std::fs::write(vault_file(&state), SIGNED_IN).unwrap();

        let one = stand_in(&state, "wt:one");
        let two = stand_in(&state, "wt:two");
        assert_ne!(one, two, "each thread mints its own");
        assert!(one.starts_with("bakery-stand-in-claude-"), "{one}");
        assert!(!one.contains("real-access"));

        for thread in ["wt:one", "wt:two"] {
            let blob = stand_in_credential(&state, thread).unwrap();
            assert!(blob.contains(&stand_in(&state, thread)), "{blob}");
            assert!(!blob.contains("real-access"), "the seat never receives the real token: {blob}");
            assert!(!blob.contains("real-refresh") && !blob.contains("refreshToken"), "no refresh token, by value or by key: {blob}");
            assert!(blob.contains("\"subscriptionType\":\"max\""), "{blob}");
            assert!(blob.contains("user:inference"), "the plan's shape is mirrored: {blob}");
            assert!(blob.contains("\"rateLimitTier\":\"max_20x\""), "the allowlisted tier reaches the seat: {blob}");
            assert!(!blob.contains("planMeta"), "a field not on the allowlist stays out of the seat: {blob}");
        }
    }

    /// A field Claude Code may one day add that holds a secret (an id token,
    /// a session or org key) stays in the node's own vault record but never
    /// reaches a room: only the allowlist and the named plan fields cross
    /// into a seat's stand-in.
    #[test]
    fn an_unknown_field_stays_in_the_vault_but_never_reaches_the_stand_in() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        std::fs::create_dir_all(vault_dir(&state)).unwrap();
        // Synthetic: never a real token. "idToken" stands for a future
        // secret-bearing field not on the allowlist.
        let record = r#"{"claudeAiOauth":{"accessToken":"real-access","refreshToken":"real-refresh","expiresAt":99999999999999,"subscriptionType":"max","scopes":["user:inference"],"rateLimitTier":"max_20x","idToken":"synthetic-id-token"}}"#;
        std::fs::write(vault_file(&state), record).unwrap();

        // The node's own record keeps it (the vault is the node's).
        let stored = stored(&state).unwrap();
        assert_eq!(stored.extra.get("idToken").and_then(|v| v.as_str()), Some("synthetic-id-token"));

        // The seat's stand-in does not carry it; the allowlisted tier still does.
        let blob = stand_in_credential(&state, "wt:one").unwrap();
        assert!(!blob.contains("idToken") && !blob.contains("synthetic-id-token"), "a field off the allowlist never reaches the room: {blob}");
        assert!(blob.contains("\"rateLimitTier\":\"max_20x\""), "the allowlisted tier still crosses: {blob}");
    }

    #[test]
    fn no_sign_in_means_no_stand_in() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_at(dir.path());
        assert!(access_token(&state).is_none());
        assert!(stand_in_credential(&state, "wt:none").is_none());
    }
}