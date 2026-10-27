//! The swap at the door, after OpenShell's design: a thread's sandbox never
//! holds a real sign-in. Each sign-in the operator turns on for a project
//! (its setup's `sign_ins`) gives the project's rooms a stand-in instead, and
//! at the thread's door a request to one of that sign-in's hosts has the
//! stand-in replaced by the real credential, which only the node holds. A
//! stand-in bound anywhere else never leaves the Mac: one left in a header is
//! refused, one in a body is blanked out and the request goes on.
//!
//! Tools that take a base URL (the Anthropic, OpenAI, xAI and Gemini SDKs)
//! are pointed at the door, `http://127.0.0.1:<door>/to/<host>/…`, which
//! sends the request on over TLS. Google's tools take none: their rooms
//! find a metadata server at the door (`GCE_METADATA_HOST`) that hands out
//! a stand-in token, and their HTTPS to Google's hosts is answered by the
//! door itself under the node's own authority (`swap_tls`), which those
//! rooms trust, so the stand-in can be made real there. Where the real
//! credential comes from is a source per sign-in: the node's own keys, the
//! operator's own gcloud sign-in for Google, or one plugged in with [`plug`]
//! (a refreshed sign-in, say). Nothing here is model-specific.
//!
//! With the operator's own Google sign-in, reads go through; a change to
//! their cloud (a deploy, a write, a delete: anything but GET and HEAD)
//! waits at the door on a card for them, as a room's git push does, unless
//! they let the conversation change that Google Cloud project from now on
//! (`grants`), which never covers a delete.
//!
//! The operator's Claude subscription is not one of those checkboxes. A
//! claude-native seat holds a stand-in minted for its thread
//! (`claude_sign_in`), and this door makes that stand-in the real access
//! token only toward `api.anthropic.com`, and only while the thread still
//! has a claude-native seat. No value, or the wrong one, is refused.

use crate::state::AppState;
use axum::{
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// What every stand-in starts with: one left in a request after the swap
/// means a sign-in that isn't this host's, and the request is refused.
const MARK: &str = "bakery-stand-in-";

/// A sign-in a project's rooms may use through their door.
pub(crate) struct SignIn {
    /// As the operator turns it on for a project.
    pub(crate) name: &'static str,
    /// The hosts it may be sent to, and no other.
    pub(crate) hosts: &'static [&'static str],
    /// The variables a room gets its stand-in under.
    pub(crate) keys: &'static [&'static str],
    /// The variables that point a room's tools at its door instead of the
    /// host: the variable, the host, the path the tools expect after it.
    pub(crate) base_urls: &'static [(&'static str, &'static str, &'static str)],
    /// Whether the door answers its hosts' TLS itself (tools with no base
    /// URL).
    pub(crate) intercept: bool,
}

/// Whether `host` is one of `hosts`, or under one written with a leading dot.
fn host_listed(hosts: &[&str], host: &str) -> bool {
    hosts.iter().any(|h| match h.strip_prefix('.') {
        Some(domain) => host.ends_with(h) && host.len() > domain.len() + 1,
        None => host == *h,
    })
}

impl SignIn {
    /// Whether it may be sent to `host`.
    pub(crate) fn serves(&self, host: &str) -> bool {
        host_listed(self.hosts, host)
    }
}

/// A per-thread minted sign-in for one seat model, under one provider — the
/// operator's subscription, or their own key with a vendor the seat talks to
/// directly (console, OpenRouter). Not a project checkbox: the door turns it
/// on when that model (and, for Claude native, that provider) is assigned on
/// the thread. The stand-in is minted per thread. The vendor's vault and
/// credential file (when it has one) stay with that vendor.
pub(crate) struct Subscription {
    /// Stand-in namespace. Not a checkbox name ([`names`] does not list
    /// these). Distinct per row, even when two rows share a `seat_model`.
    name: &'static str,
    /// Seat model that turns this row on. More than one row may share it —
    /// Claude native's three providers all do — distinguished by `provider`.
    seat_model: &'static str,
    /// `crate::claude_providers::Provider::id`, or `"subscription"` for a
    /// vendor with only the one. [`subscription_for_model`] picks this one.
    provider: &'static str,
    hosts: &'static [&'static str],
    /// Where the seat holds the minted access-token stand-in.
    token_env: &'static str,
    /// The variable, the host, and the path the seat's tools expect after it.
    base_url: (&'static str, &'static str, &'static str),
    /// Env keys that would take this seat off this row (a raw key or token
    /// for the same vendor, under a DIFFERENT variable, that Claude Code
    /// would prefer over this row's own).
    strip_env: &'static [&'static str],
    /// Literal pairs always present and always pinned (never shadowed by an
    /// inherited grant) — e.g. OpenRouter's docs ask for an explicitly empty
    /// `ANTHROPIC_API_KEY`, so Claude Code never falls back to it.
    extra_env: &'static [(&'static str, &'static str)],
    access_token: fn(&AppState) -> Option<String>,
    /// Vendor credential blob whose access token is this thread's stand-in.
    /// `None` always, for a row with no vault file of its own — a plain API
    /// key or bearer token authenticates by env alone.
    credential: fn(&AppState, &str) -> Option<String>,
    /// Replies on these paths lose identity keys. Inference paths are not
    /// listed. Set false on a row if a live turn shows that engine will not
    /// run without those fields; the door then leaves that row's replies as
    /// the vendor sent them.
    strip_identity: bool,
    identity_paths: &'static [&'static str],
}

fn no_vendor_credential(_state: &AppState, _stream_id: &str) -> Option<String> {
    None
}

/// `secrets::load().anthropic_api_key` — the same key the operator already
/// saves for Settings' plain `claude` (API-key) model, reused so a console
/// Claude native seat never asks them to paste it twice.
fn console_api_key(_state: &AppState) -> Option<String> {
    bakery_agent::secrets::load().anthropic_api_key
}

/// `secrets::load().openrouter_api_key` — the OpenRouter key saved the same
/// way (`POST /auth/api-key {"provider":"openrouter", ...}`).
fn openrouter_api_key(_state: &AppState) -> Option<String> {
    bakery_agent::secrets::load().openrouter_api_key
}

/// Claude native's three providers, then another vendor's own row (its
/// vault, its file format, its hosts, and the seat model that should
/// receive it).
const SUBSCRIPTIONS: &[Subscription] = &[
    Subscription {
        name: "claude",
        seat_model: crate::claude_native::ACCOUNT_ID,
        provider: "subscription",
        hosts: &["api.anthropic.com"],
        token_env: "CLAUDE_CODE_OAUTH_TOKEN",
        base_url: ("ANTHROPIC_BASE_URL", "api.anthropic.com", ""),
        strip_env: &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        extra_env: &[],
        access_token: crate::claude_sign_in::access_token,
        credential: crate::claude_sign_in::stand_in_credential,
        strip_identity: true,
        // Profile and account bootstrap. Not `/v1/messages`. jcode records the
        // official CLI treating bootstrap, account settings, and grove as
        // nonessential: a failure there does not stop inference. Stripping
        // identity keys leaves subscription and limit fields in place.
        identity_paths: &[
            "/api/oauth/profile",
            "/api/oauth/account",
            "/api/claude_cli/bootstrap",
            "/api/claude_code_grove",
        ],
    },
    // The operator's own Anthropic console key — the same one `claude`
    // (Bakery's plain API-key model) already uses. `ANTHROPIC_API_KEY` is
    // Claude Code's documented env for this; a real OAuth sign-in or an
    // OpenRouter bearer would both outrank it, so both are stripped.
    Subscription {
        name: "claude-console",
        seat_model: crate::claude_native::ACCOUNT_ID,
        provider: "console",
        hosts: &["api.anthropic.com"],
        token_env: "ANTHROPIC_API_KEY",
        base_url: ("ANTHROPIC_BASE_URL", "api.anthropic.com", ""),
        strip_env: &["ANTHROPIC_AUTH_TOKEN", "CLAUDE_CODE_OAUTH_TOKEN"],
        extra_env: &[],
        access_token: console_api_key,
        credential: no_vendor_credential,
        strip_identity: false,
        identity_paths: &[],
    },
    // OpenRouter's own Claude Code guide (openrouter.ai/docs/guides/guides/
    // claude-code-integration, verified 2026): ANTHROPIC_BASE_URL =
    // "https://openrouter.ai/api" (door-swapped as host "openrouter.ai",
    // path "/api"), the OpenRouter key as a bearer in ANTHROPIC_AUTH_TOKEN,
    // and ANTHROPIC_API_KEY explicitly blanked so Claude Code never falls
    // back to direct Anthropic auth (an unset key, the docs warn, can still
    // be picked up from elsewhere).
    Subscription {
        name: "claude-openrouter",
        seat_model: crate::claude_native::ACCOUNT_ID,
        provider: "openrouter",
        hosts: &["openrouter.ai"],
        token_env: "ANTHROPIC_AUTH_TOKEN",
        base_url: ("ANTHROPIC_BASE_URL", "openrouter.ai", "/api"),
        strip_env: &["ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"],
        extra_env: &[("ANTHROPIC_API_KEY", "")],
        access_token: openrouter_api_key,
        credential: no_vendor_credential,
        strip_identity: false,
        identity_paths: &[],
    },
    // The grok CLI (the xAI binary). The seat holds a stand-in auth.json and
    // is pointed at this door for cli-chat-proxy.grok.com only. api.x.ai is
    // the HTTP API-key sign-in, not this row. Cursor's hosts are not here.
    Subscription {
        name: "grok-cli",
        seat_model: "acp:grok",
        provider: "subscription",
        hosts: &[crate::grok_cli::CHAT_HOST],
        token_env: "GROK_CLI_STAND_IN",
        base_url: ("GROK_CLI_CHAT_PROXY_BASE_URL", crate::grok_cli::CHAT_HOST, "/v1"),
        strip_env: &["XAI_API_KEY", "GROK_CODE_XAI_API_KEY"],
        extra_env: &[],
        access_token: crate::grok_cli::access_token,
        credential: no_vendor_credential,
        strip_identity: false,
        identity_paths: &[],
    },
];

impl Subscription {
    fn serves(&self, host: &str) -> bool {
        host_listed(self.hosts, host)
    }

    pub(crate) fn name(&self) -> &'static str {
        self.name
    }

    fn model_matches(&self, state: &AppState, model_id: &str) -> bool {
        if self.name == "grok-cli" {
            return crate::grok_cli::serves_model(state, model_id);
        }
        model_id.eq_ignore_ascii_case(self.seat_model)
    }

    /// Whether some seat on this thread is on this row RIGHT NOW: its model
    /// AND, when more than one row shares that model (Claude native's
    /// three), its provider too. Checked under one lock (`for_seat_locked`,
    /// never the double-locking `for_seat`) so a stand-in minted for a
    /// provider the seat has since switched away from can never be swapped
    /// real again by a leftover request — switching is a clean break, not a
    /// second live row.
    fn on_thread(&self, state: &AppState, stream_id: &str) -> bool {
        let heart = state.heart.lock().unwrap_or_else(|e| e.into_inner());
        [
            crate::node::Seat::Orchestrator,
            crate::node::Seat::Worker,
            crate::node::Seat::Baker,
        ]
        .into_iter()
        .any(|seat| {
            crate::state::latest_seat_model_id(&heart, stream_id, seat)
                .ok()
                .flatten()
                .is_some_and(|id| self.model_matches(state, &id))
                && crate::claude_providers::for_seat_locked(&heart, stream_id, seat).id() == self.provider
        })
    }

    /// Whether `key` may be handed to this seat. A real token in
    /// [`Self::token_env`] is dropped; only the minted stand-in stays.
    pub(crate) fn keep_env(&self, key: &str, value: &str) -> bool {
        if self.strip_env.contains(&key) {
            return false;
        }
        if key == self.token_env {
            return value.starts_with(&minted_prefix(self.name));
        }
        true
    }

    pub(crate) fn pins_env(&self, key: &str) -> bool {
        key == self.token_env
            || key == self.base_url.0
            || self.extra_env.iter().any(|(k, _)| *k == key)
    }

    pub(crate) fn signed_in(&self, state: &AppState) -> bool {
        (self.access_token)(state).is_some()
    }
}

/// The row for `model_id` under `provider`
/// (`crate::claude_providers::Provider::id`, or any string for a vendor with
/// only one row — see [`subscription_for_model`]).
pub(crate) fn subscription_for(model_id: &str, provider: &str) -> Option<&'static Subscription> {
    SUBSCRIPTIONS.iter().find(|sub| {
        sub.provider == provider
            && (sub.seat_model.eq_ignore_ascii_case(model_id)
                || (sub.name == "grok-cli" && crate::grok_cli::known_model(model_id)))
    })
}

/// [`subscription_for`], plus a grok CLI plug whose id is not one of the
/// documented ones but whose binary is `grok`.
pub(crate) fn subscription_for_seat(
    state: &AppState,
    model_id: &str,
    provider: &str,
) -> Option<&'static Subscription> {
    if crate::grok_cli::serves_model(state, model_id) {
        return subscription_for("acp:grok", provider);
    }
    subscription_for(model_id, provider)
}

/// The subscription row for `model_id` — the provider-less lookup every
/// caller used before a model could have more than one provider. Still
/// exactly that row (Claude native's own OAuth sign-in): a caller that
/// wants a DIFFERENT provider's row must give it via [`subscription_for`].
pub(crate) fn subscription_for_model(model_id: &str) -> Option<&'static Subscription> {
    subscription_for(model_id, "subscription")
}

/// What one subscription seat holds: its minted stand-in, and its tools
/// pointed at this thread's door. Empty when `model_id` has no subscription.
pub(crate) fn minted_for_model(state: &AppState, model_id: &str, stream_id: &str) -> Option<String> {
    subscription_for_model(model_id).map(|sub| minted(state, sub.name, stream_id))
}

/// `http://127.0.0.1:<door>/to`, the host seat's door.
pub(crate) fn subscription_origin_host(door: u16) -> String {
    format!("http://127.0.0.1:{door}/to")
}

/// The seats' gateway, with the thread in the path. A container cannot
/// reach the host's loopback door. The stand-in still has to match this
/// thread, so naming another thread here does not spend its subscription.
pub(crate) fn subscription_origin_container(gateway: std::net::Ipv4Addr, stream_id: &str) -> String {
    format!(
        "http://{gateway}:{}/s/{}/to",
        crate::seat_gateway::PORT,
        path_segment(stream_id)
    )
}

fn path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(crate) fn subscription_env(
    state: &AppState,
    stream_id: &str,
    model_id: &str,
    provider: &str,
    door: u16,
) -> Vec<(String, String)> {
    subscription_env_at(state, stream_id, model_id, provider, &subscription_origin_host(door))
}

pub(crate) fn subscription_env_at(
    state: &AppState,
    stream_id: &str,
    model_id: &str,
    provider: &str,
    origin: &str,
) -> Vec<(String, String)> {
    let Some(sub) = subscription_for(model_id, provider) else {
        return Vec::new();
    };
    let (var, host, path) = sub.base_url;
    let mut env = vec![
        (sub.token_env.to_string(), minted(state, sub.name, stream_id)),
        (var.to_string(), format!("{origin}/{host}{path}")),
    ];
    env.extend(sub.extra_env.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    env
}

/// The vendor credential blob for this seat's subscription, if the operator
/// is signed in. `None` when the model has no subscription or no sign-in.
pub(crate) fn subscription_credential(state: &AppState, stream_id: &str, model_id: &str) -> Option<String> {
    let sub = subscription_for_model(model_id)?;
    (sub.credential)(state, stream_id)
}

pub(crate) const SIGN_INS: &[SignIn] = &[
    SignIn {
        name: "anthropic",
        hosts: &["api.anthropic.com"],
        keys: &["ANTHROPIC_API_KEY"],
        base_urls: &[("ANTHROPIC_BASE_URL", "api.anthropic.com", "")],
        intercept: false,
    },
    SignIn {
        name: "openai",
        hosts: &["api.openai.com"],
        keys: &["OPENAI_API_KEY"],
        base_urls: &[("OPENAI_BASE_URL", "api.openai.com", "/v1")],
        intercept: false,
    },
    SignIn {
        name: "xai",
        hosts: &["api.x.ai"],
        keys: &["XAI_API_KEY"],
        base_urls: &[("XAI_BASE_URL", "api.x.ai", "/v1")],
        intercept: false,
    },
    SignIn {
        name: "gemini",
        hosts: &["generativelanguage.googleapis.com"],
        keys: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        base_urls: &[("GOOGLE_GEMINI_BASE_URL", "generativelanguage.googleapis.com", "")],
        intercept: false,
    },
    // The operator's own gcloud sign-in, for Google's SDKs and gcloud.
    SignIn {
        name: "google",
        hosts: &[".googleapis.com"],
        keys: &[],
        base_urls: &[],
        intercept: true,
    },
];

/// The names a project's setup may turn on.
pub(crate) fn names() -> Vec<&'static str> {
    SIGN_INS.iter().map(|s| s.name).collect()
}

/// What a room holds in place of a checkbox sign-in. The same string for
/// every thread. A subscription uses [`minted`] instead.
pub(crate) fn stand_in(name: &str) -> String {
    format!("{MARK}{name}")
}

fn minted_prefix(name: &str) -> String {
    format!("{MARK}{name}-")
}

/// The node's seed for per-thread subscription stand-ins. 32 random bytes
/// in `swap/subscription-seed`, mode 0600. It is not an access token and
/// not a refresh token. Anyone who can read it can compute a thread's
/// stand-in, so seats are denied the `swap` directory that holds it.
fn subscription_seed(state: &AppState) -> [u8; 32] {
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let path = state.base.join("swap").join("subscription-seed");
    if let Ok(existing) = std::fs::read(&path) {
        if existing.len() == 32 {
            let mut seed = [0u8; 32];
            seed.copy_from_slice(&existing);
            return seed;
        }
    }
    let mut bytes = [0u8; 32];
    let drew = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !drew {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes = *blake3::hash(format!("{nanos}-{}", std::process::id()).as_bytes()).as_bytes();
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
    {
        let _ = file.write_all(&bytes);
    }
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    bytes
}

/// This thread's stand-in for subscription `name`. It is not in the source,
/// it differs per thread, and only this thread's door will make it real.
pub(crate) fn minted(state: &AppState, name: &str, stream_id: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&subscription_seed(state));
    hasher.update(name.as_bytes());
    hasher.update(b"\0");
    hasher.update(stream_id.as_bytes());
    let digest = hasher.finalize().to_hex();
    format!("{}{}", minted_prefix(name), &digest[..16])
}

/// Where a sign-in's real credential comes from, when not the node's own
/// keys: plugged in by the code that keeps it (a refreshed sign-in).
pub(crate) type Source = Arc<dyn Fn(&AppState) -> Option<String> + Send + Sync>;

fn sources() -> &'static Mutex<HashMap<String, Source>> {
    static SOURCES: OnceLock<Mutex<HashMap<String, Source>>> = OnceLock::new();
    SOURCES.get_or_init(Default::default)
}

/// From now on `name`'s real credential comes from `source`.
#[allow(dead_code)] // sign-ins that refresh plug in here once the operator says so
pub(crate) fn plug(name: &str, source: Source) {
    sources().lock().unwrap().insert(name.to_string(), source);
}

/// The sign-in's real credential, as only the node holds it: its plugged
/// source, else the node's own key (its secrets, then its environment, as
/// the Bakery's own model calls find it).
fn real(state: &AppState, name: &str) -> Option<String> {
    let plugged = sources().lock().unwrap().get(name).cloned();
    if let Some(source) = plugged {
        return source(state);
    }
    if name == "google" {
        return google_token(state);
    }
    let secrets = bakery_agent::secrets::load();
    let stored = match name {
        "anthropic" => secrets.anthropic_api_key,
        "xai" => secrets.xai_api_key,
        "gemini" => secrets.gemini_api_key,
        _ => None,
    };
    let env = SIGN_INS
        .iter()
        .find(|s| s.name == name)
        .into_iter()
        .flat_map(|s| s.keys.iter())
        .find_map(|key| std::env::var(key).ok());
    stored
        .or(env)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// The sign-ins a thread's project turned on.
fn enabled(state: &AppState, stream_id: &str) -> Vec<&'static SignIn> {
    let project_id = {
        let heart = state.heart.lock().unwrap();
        crate::state::socket_project(&heart, stream_id).0
    };
    let on = project_id
        .and_then(|p| crate::catalog::project_by_id(state, &p))
        .map(|p| p.stacks.sign_ins)
        .unwrap_or_default();
    SIGN_INS
        .iter()
        .filter(|s| on.iter().any(|n| n == s.name))
        .collect()
}

/// What a thread's agent is told of its project's sign-ins, once a session.
pub(crate) fn session_line(state: &AppState, stream_id: &str) -> Option<String> {
    let on = enabled(state, stream_id);
    if on.is_empty() {
        return None;
    }
    let names: Vec<&str> = on.iter().map(|s| s.name).collect();
    let google = if names.contains(&"google") {
        "; with the operator's Google sign-in, reads go through and each change to their cloud (anything but GET) waits for their approval: when Google answers that it's waiting for the operator, tell them what it is and run it again once they approve"
    } else {
        ""
    };
    Some(format!(
        "sign-ins: {} through this thread's door; the keys in your environment are stand-ins it makes real on the way, so leave them and their base URLs as they are, and never put a real key anywhere{google}",
        names.join(", ")
    ))
}

/// What a thread's rooms get for its project's sign-ins: each stand-in,
/// each base URL pointed at the thread's door; for Google, the metadata
/// server at its door and the trust in the door's authority.
pub(crate) fn env_for(state: &AppState, stream_id: &str, door: u16) -> Vec<(String, String)> {
    let mut env = Vec::new();
    let on = enabled(state, stream_id);
    if on.iter().any(|s| s.intercept) && crate::swap_tls::ca(&state.base).is_ok() {
        let bundle = crate::swap_tls::bundle(&state.base).display().to_string();
        for name in [
            "SSL_CERT_FILE",
            "REQUESTS_CA_BUNDLE",
            "CURL_CA_BUNDLE",
            "GIT_SSL_CAINFO",
            "CLOUDSDK_CORE_CUSTOM_CA_CERTS_FILE",
        ] {
            env.push((name.to_string(), bundle.clone()));
        }
        let ca = crate::swap_tls::ca_pem(&state.base).display().to_string();
        env.push(("NODE_EXTRA_CA_CERTS".to_string(), ca));
    }
    if on.iter().any(|s| s.name == "google") {
        for name in ["GCE_METADATA_HOST", "GCE_METADATA_IP"] {
            env.push((name.to_string(), format!("127.0.0.1:{door}")));
        }
        env.push(("METADATA_SERVER_DETECTION".to_string(), "assume-present".to_string()));
    }
    for sign_in in on {
        for key in sign_in.keys {
            env.push((key.to_string(), stand_in(sign_in.name)));
        }
        for (name, host, path) in sign_in.base_urls {
            env.push((
                name.to_string(),
                format!("http://127.0.0.1:{door}/to/{host}{path}"),
            ));
        }
    }
    env
}

type Refused = (StatusCode, String);

/// `text` with each stand-in of a sign-in bound to `host` made real (`real`
/// names each sign-in's credential). `minted_tokens` are this thread's
/// subscription stand-ins that `host` may receive, already paired with the
/// real access token. A stand-in left over is refused, never sent.
fn swapped(
    text: &str,
    host: &str,
    on: &[&SignIn],
    real: &dyn Fn(&str) -> Option<String>,
    minted_tokens: &[(&str, &str)],
) -> Result<String, Refused> {
    if !text.contains(MARK) {
        return Ok(text.to_string());
    }
    let mut out = text.to_string();
    for sign_in in on.iter().filter(|s| s.serves(host)) {
        let stand_in = stand_in(sign_in.name);
        if out.contains(&stand_in) {
            let credential = real(sign_in.name).ok_or_else(|| {
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("the operator hasn't given the Bakery a {} key", sign_in.name),
                )
            })?;
            out = out.replace(&stand_in, &credential);
        }
    }
    for (stand_in, token) in minted_tokens {
        if !stand_in.is_empty() && out.contains(stand_in) {
            out = out.replace(stand_in, token);
        }
    }
    if out.contains(MARK) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("a stand-in that isn't {host}'s to receive: never sent"),
        ));
    }
    Ok(out)
}

/// Whether the request carries `name`'s stand-in, in a header or its query.
fn carries(parts: &axum::http::request::Parts, name: &str) -> bool {
    let stand_in = stand_in(name);
    parts.uri.query().is_some_and(|q| q.contains(&stand_in))
        || parts
            .headers
            .values()
            .any(|v| v.to_str().is_ok_and(|v| v.contains(&stand_in)))
}

/// A request body on its way to `host`: every stand-in in it blanked to a
/// filler of its own length, so a stand-in never leaves the Mac and the
/// body's length — and so its Content-Length — never changes. The real
/// credential is a header's to carry, never a body's, so a body's stand-in
/// is only ever removed, never made real. `None` when the body held none.
///
/// The door can only blank a stand-in it can read, so a body in any
/// `content-encoding` but identity is refused outright, the way a header's
/// leftover stand-in is — and checked before the scan: a compressed body's
/// bytes aren't the content's own, so a stand-in inside it would not spell
/// MARK in those bytes and would otherwise slip past a raw scan and go out.
fn blank_body(body: &[u8], content_encodings: &[&str]) -> Result<Option<Vec<u8>>, Refused> {
    // Every Content-Encoding line, and every value on each (a header may be
    // repeated or comma-joined): all must be identity, or absent. A single
    // non-identity anywhere — "identity, gzip" or a second "gzip" line — is
    // an encoded body the door can't scan, and is refused.
    let identity = content_encodings.iter().flat_map(|line| line.split(',')).all(|part| {
        let part = part.trim();
        part.is_empty() || part.eq_ignore_ascii_case("identity")
    });
    if !identity {
        return Err((
            StatusCode::FORBIDDEN,
            "a request body in a content-encoding the Bakery can't scan for a stand-in: never sent".to_string(),
        ));
    }
    let mark = MARK.as_bytes();
    if !body.windows(mark.len()).any(|w| w == mark) {
        return Ok(None);
    }
    // `bakery-redacted-…` of the removed stand-in's own byte length, so the
    // body's length — and its Content-Length — holds. No stand-in can
    // survive, so no second scan is needed: the run replaced is MARK and the
    // token bytes after it; the filler is no sign-in's and never spells MARK
    // (its `bakery-` is always followed by `redacted`); the body bytes before
    // a run can't reach a MARK through the filler; and a run ends on a
    // non-token byte, which no MARK can cross.
    const BAND: &[u8] = b"bakery-redacted-";
    let in_token = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'_';
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i..].starts_with(mark) {
            let mut end = i + mark.len();
            while end < body.len() && in_token(body[end]) {
                end += 1;
            }
            out.extend((0..end - i).map(|k| BAND[k % BAND.len()]));
            i = end;
        } else {
            out.push(body[i]);
            i += 1;
        }
    }
    Ok(Some(out))
}

/// Every Content-Encoding value a request carries — each header line's own,
/// all of them, not just the first, since a second line could hide an
/// encoding the door can't scan. A line whose bytes aren't readable ASCII
/// becomes `"?"`, which is not identity, so an unreadable encoding is
/// refused rather than skipped (the check never fails open).
fn request_encodings(headers: &axum::http::HeaderMap) -> Vec<&str> {
    headers
        .get_all(axum::http::header::CONTENT_ENCODING)
        .iter()
        .map(|v| v.to_str().unwrap_or("?"))
        .collect()
}

/// The request, its stand-ins made real for `host` in headers and query
/// (a leftover there is refused), and blanked in its body. `Some` is the
/// body rewritten; `None`, the body unchanged.
fn swap_request(
    parts: &mut axum::http::request::Parts,
    body: &[u8],
    host: &str,
    on: &[&SignIn],
    real: &dyn Fn(&str) -> Option<String>,
    minted_tokens: &[(&str, &str)],
) -> Result<Option<Vec<u8>>, Refused> {
    let encodings = request_encodings(&parts.headers);
    let body_out = blank_body(body, &encodings)?;
    for value in parts.headers.values_mut() {
        let Ok(text) = value.to_str() else {
            continue;
        };
        let made = swapped(text, host, on, real, minted_tokens)?;
        if made != text {
            let mut new =
                HeaderValue::from_str(&made).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
            new.set_sensitive(true);
            *value = new;
        }
    }
    if let Some(query) = parts.uri.query() {
        let made = swapped(query, host, on, real, minted_tokens)?;
        if made != query {
            let path = format!("{}?{made}", parts.uri.path());
            parts.uri = path
                .parse()
                .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}")))?;
        }
    }
    Ok(body_out)
}

/// The TLS the door's requests leave with: the public roots.
fn tls() -> &'static tokio_rustls::TlsConnector {
    static TLS: OnceLock<tokio_rustls::TlsConnector> = OnceLock::new();
    TLS.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("rustls's default versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(config))
    })
}

/// Hosts a test answers on loopback instead, in plain http.
#[cfg(test)]
fn test_hosts() -> &'static Mutex<HashMap<String, u16>> {
    static HOSTS: OnceLock<Mutex<HashMap<String, u16>>> = OnceLock::new();
    HOSTS.get_or_init(Default::default)
}

/// Sends `req` to `host` over TLS and brings its answer back as it streams.
async fn send(host: &str, req: axum::http::Request<Body>) -> Result<Response, String> {
    use hyper_util::rt::TokioIo;
    #[cfg(test)]
    let test_port = test_hosts().lock().unwrap().get(host).copied();
    #[cfg(test)]
    if let Some(port) = test_port {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|e| e.to_string())?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tcp))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        let answer = sender.send_request(req).await.map_err(|e| e.to_string())?;
        return Ok(answer.map(Body::new));
    }
    let tcp = tokio::net::TcpStream::connect((host, 443))
        .await
        .map_err(|e| format!("{host}: {e}"))?;
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|e| e.to_string())?;
    let stream = tls()
        .connect(name, tcp)
        .await
        .map_err(|e| format!("{host}: {e}"))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let answer = sender.send_request(req).await.map_err(|e| e.to_string())?;
    Ok(answer.map(Body::new))
}

/// The most a request's body may be, read whole to look for stand-ins.
const MAX_BODY: usize = 64 << 20;

/// Whether the door answers `authority`'s TLS itself for the thread: one
/// of its project's intercepted sign-in hosts, on 443.
pub(crate) fn intercepts(state: &AppState, stream_id: &str, authority: &str) -> bool {
    let Some((host, "443")) = authority.rsplit_once(':') else {
        return false;
    };
    enabled(state, stream_id)
        .iter()
        .any(|s| s.intercept && s.serves(host))
}

/// One request on a connection the door answered for `host`
/// ([`crate::swap_tls::serve`]): sent on as [`to_host`] sends its own.
pub(crate) async fn intercepted(state: &AppState, stream_id: &str, host: &str, req: axum::http::Request<Body>) -> Response {
    let on = {
        let (state, stream_id) = (state.clone(), stream_id.to_string());
        crate::actor::spawn_blocking(move || enabled(&state, &stream_id)).await.unwrap_or_default()
    };
    let (parts, body) = req.into_parts();
    match swap_and_send(state, stream_id, on, host, parts, body).await {
        Ok(answer) => answer,
        Err((status, why)) => google_error(status, &why),
    }
}

/// A refusal as Google's APIs word theirs, so their tools show the reason.
fn google_error(status: StatusCode, why: &str) -> Response {
    let code = match status {
        StatusCode::FORBIDDEN => "PERMISSION_DENIED",
        StatusCode::BAD_REQUEST | StatusCode::PAYLOAD_TOO_LARGE => "INVALID_ARGUMENT",
        _ => "UNAVAILABLE",
    };
    let body = json!({ "error": { "code": status.as_u16(), "message": why, "status": code } });
    (status, Json(body)).into_response()
}

/// A change a room sent to Google's APIs with the operator's own sign-in,
/// as its card shows it.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub(crate) struct GoogleChange {
    pub(crate) stream_id: String,
    /// `POST`, `PUT`, `PATCH` or `DELETE`.
    pub(crate) method: String,
    /// Where it goes, as the room sent it: stand-ins, never a real token.
    pub(crate) url: String,
    /// What it sends: its fields' names, never their values.
    pub(crate) sends: String,
}

/// The card's account of a change: where it goes and what it sends.
fn google_change(stream_id: &str, host: &str, parts: &axum::http::request::Parts, body: &[u8]) -> Value {
    let sends = match serde_json::from_slice::<Value>(body) {
        _ if body.is_empty() => "nothing".to_string(),
        Ok(Value::Object(fields)) => fields.keys().cloned().collect::<Vec<_>>().join(", "),
        _ => parts
            .headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("data")
            .to_string(),
    };
    let path = parts.uri.path_and_query().map_or("/", |pq| pq.as_str());
    json!({
        "stream_id": stream_id,
        "method": parts.method.as_str(),
        "url": format!("https://{host}{path}"),
        "sends": sends,
    })
}

/// `POST /google/change`: what a door asks for a room's change to Google
/// Cloud. Approved, or covered by a grant, the door sends the request it
/// holds; this sends nothing itself.
pub(crate) async fn change_route(Json(change): Json<GoogleChange>) -> Json<String> {
    Json(format!(
        "approved: {} {} (sending {}) goes out when {}'s door sends it",
        change.method, change.url, change.sends, change.stream_id
    ))
}

/// `/to/<host>/<path>` at a thread's door: a request for one of its
/// project's sign-in hosts, sent on with its stand-ins made real.
pub(crate) async fn to_host(State(door): State<crate::doors::Door>, req: Request) -> Response {
    for_thread(door.state, door.stream_id, req).await
}

pub(crate) async fn for_thread(state: AppState, stream_id: String, req: Request) -> Response {
    let door = crate::doors::Door { state, stream_id };
    match forward(&door, req).await {
        Ok(answer) => answer,
        Err((status, why)) => (status, why).into_response(),
    }
}

/// A container seat's plain HTTP to its gateway: `/s/<thread>/to/<host>/...`.
/// Axum has already decoded the thread. The path is rewritten to the door's
/// `/to/<host>/...` shape.
pub(crate) async fn gateway_to_host(
    State(state): State<AppState>,
    axum::extract::Path((stream_id, rest)): axum::extract::Path<(String, String)>,
    mut req: Request,
) -> Response {
    let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
    if let Ok(uri) = format!("/to/{rest}{query}").parse() {
        *req.uri_mut() = uri;
    }
    for_thread(state, stream_id, req).await
}

/// Keys that name a person or an account. Removed from subscription
/// identity replies only. A bare `name` or `id` is left, so a model list
/// inside a bootstrap body keeps its names.
const IDENTITY_KEYS: &[&str] = &[
    "email",
    "emailaddress",
    "email_address",
    "displayname",
    "display_name",
    "fullname",
    "full_name",
    "accountname",
    "username",
    "given_name",
    "family_name",
    "organizationuuid",
    "organization_uuid",
    "organizationname",
    "organization_name",
    "orgname",
    "orguuid",
    "org_uuid",
    "accountuuid",
    "account_uuid",
    "accountid",
    "account_id",
    "userid",
    "user_id",
    "useruuid",
    "user_uuid",
    "avatar",
    "avatarurl",
    "avatar_url",
    "picture",
    "photo",
    "image_url",
];

fn identity_path(host: &str, path: &str) -> bool {
    SUBSCRIPTIONS.iter().any(|sub| {
        sub.strip_identity
            && sub.serves(host)
            && sub.identity_paths.iter().any(|piece| path.contains(piece))
    })
}

fn strip_identity_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|key, _| !IDENTITY_KEYS.iter().any(|id| key.eq_ignore_ascii_case(id)));
            for child in map.values_mut() {
                strip_identity_keys(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_identity_keys(item);
            }
        }
        _ => {}
    }
}

async fn strip_identity_reply(response: Response) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                "the identity reply could not be read",
            )
                .into_response();
        }
    };
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    strip_identity_keys(&mut value);
    let next = serde_json::to_vec(&value).unwrap_or_else(|_| bytes.to_vec());
    if let Ok(len) = HeaderValue::from_str(&next.len().to_string()) {
        parts.headers.insert(axum::http::header::CONTENT_LENGTH, len);
    }
    Response::from_parts(parts, Body::from(next))
}

fn carries_value_text(query: &str, headers: &[String], value: &str) -> bool {
    !value.is_empty() && (query.contains(value) || headers.iter().any(|header| header.contains(value)))
}

fn carries_value(parts: &axum::http::request::Parts, value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    parts.uri.query().is_some_and(|query| query.contains(value))
        || parts
            .headers
            .values()
            .any(|header| header.to_str().is_ok_and(|text| text.contains(value)))
}

/// Subscription stand-ins this request actually carries, paired with the
/// real access token. Only a subscription whose seat is on this thread, and
/// whose hosts include `host`.
fn minted_tokens_for(
    state: &AppState,
    stream_id: &str,
    host: &str,
    parts: &axum::http::request::Parts,
) -> Result<Vec<(String, String)>, Refused> {
    let mut out = Vec::new();
    for sub in SUBSCRIPTIONS
        .iter()
        .filter(|sub| sub.serves(host) && sub.on_thread(state, stream_id))
    {
        let stand_in = minted(state, sub.name, stream_id);
        if !carries_value(parts, &stand_in) {
            continue;
        }
        let token = (sub.access_token)(state).ok_or_else(|| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                format!("the operator's {} sign-in isn't available", sub.name),
            )
        })?;
        out.push((stand_in, token));
    }
    Ok(out)
}

async fn forward(door: &crate::doors::Door, req: Request) -> Result<Response, Refused> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let (mut parts, body) = req.into_parts();
    let rest = parts
        .uri
        .path()
        .strip_prefix("/to/")
        .unwrap_or_default()
        .to_string();
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host.to_string(), format!("/{path}")),
        None => (rest.clone(), "/".to_string()),
    };
    let query_text = parts.uri.query().unwrap_or("").to_string();
    let header_text: Vec<String> = parts
        .headers
        .values()
        .filter_map(|value| value.to_str().ok().map(str::to_string))
        .collect();
    let (on, presented) = {
        let (state, stream_id, host) = (door.state.clone(), door.stream_id.clone(), host.clone());
        crate::actor::spawn_blocking(move || {
            let on = enabled(&state, &stream_id);
            let presented = SUBSCRIPTIONS.iter().any(|sub| {
                sub.serves(&host)
                    && sub.on_thread(&state, &stream_id)
                    && carries_value_text(&query_text, &header_text, &minted(&state, sub.name, &stream_id))
            });
            (on, presented)
        })
        .await
        .map_err(|e| internal(e.to_string()))?
    };
    if !on.iter().any(|s| s.serves(&host)) && !presented {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{host} is none of this project's sign-in hosts"),
        ));
    }
    let query = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    parts.uri = format!("{path}{query}")
        .parse()
        .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e}")))?;
    swap_and_send(&door.state, &door.stream_id, on, &host, parts, body).await
}

/// A request for `host` (its path already the host's own): its stand-ins
/// made real, sent on over TLS, its answer brought back.
async fn swap_and_send(
    state: &AppState,
    stream_id: &str,
    on: Vec<&'static SignIn>,
    host: &str,
    parts: axum::http::request::Parts,
    body: Body,
) -> Result<Response, Refused> {
    let internal = |e: String| (StatusCode::INTERNAL_SERVER_ERROR, e);
    let mut parts = parts;
    let host = host.to_string();
    let body = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|e| (StatusCode::PAYLOAD_TOO_LARGE, e.to_string()))?;
    // A change to the operator's Google Cloud with their own sign-in waits
    // on their card, unless a grant covers it.
    let change = (!matches!(parts.method, Method::GET | Method::HEAD) && carries(&parts, "google"))
        .then(|| google_change(stream_id, &host, &parts, &body));
    if let Some(change) = change.clone() {
        let (state, stream_id) = (state.clone(), stream_id.to_string());
        crate::actor::spawn_blocking(move || crate::remote_ask::as_agent(&state, &stream_id, "google_change", &change))
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(|why| (StatusCode::FORBIDDEN, why))?;
    }
    let done = state.clone();
    let state = state.clone();
    let host_for = host.clone();
    let stream_for = stream_id.to_string();
    let (mut parts, body, used_subscription) = crate::actor::spawn_blocking(move || {
        let real = |name: &str| real(&state, name);
        let minted_owned = minted_tokens_for(&state, &stream_for, &host_for, &parts)?;
        let used_subscription = !minted_owned.is_empty();
        let minted_ref: Vec<(&str, &str)> = minted_owned
            .iter()
            .map(|(stand_in, token)| (stand_in.as_str(), token.as_str()))
            .collect();
        let blanked = swap_request(&mut parts, &body, &host_for, &on, &real, &minted_ref)?;
        let body = blanked.map(axum::body::Bytes::from).unwrap_or(body);
        Ok((parts, body, used_subscription))
    })
    .await
    .map_err(|e| internal(e.to_string()))??;
    for hop in [
        "connection",
        "proxy-connection",
        "keep-alive",
        "te",
        "trailer",
        "upgrade",
        "proxy-authorization",
    ] {
        parts.headers.remove(hop);
    }
    let host_header = HeaderValue::from_str(&host).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    parts.headers.insert(axum::http::header::HOST, host_header);
    let path = parts.uri.path().to_string();
    let req = axum::http::Request::from_parts(parts, Body::from(body));
    let mut answer = send(&host, req).await.map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    if used_subscription && identity_path(&host, &path) {
        answer = strip_identity_reply(answer).await;
    }
    // The change went out: a line in the Bakery's history of remote actions.
    if let Some(change) = change {
        let url = change["url"].as_str().unwrap_or_default();
        let line = json!({
            "what": "Google Cloud change",
            "repository": crate::grants::google_project(url).unwrap_or(host),
            "method": change["method"],
            "url": url,
            "status": answer.status().as_u16(),
        });
        let stream_id = stream_id.to_string();
        let _ = crate::actor::spawn_blocking(move || crate::remote::done(&done, Some(&stream_id), line)).await;
    }
    Ok(answer)
}

/// Where the operator's own gcloud sign-in is, as gcloud keeps it: its
/// application default credentials. Only the node reads it.
fn adc_path(state: &AppState) -> std::path::PathBuf {
    std::env::var("CLOUDSDK_CONFIG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| crate::machine_access::home_of(state).join(".config/gcloud"))
        .join("application_default_credentials.json")
}

#[derive(serde::Deserialize)]
struct Adc {
    #[serde(rename = "type")]
    kind: String,
    client_id: String,
    client_secret: String,
    refresh_token: String,
    #[serde(default)]
    quota_project_id: Option<String>,
}

fn adc(state: &AppState) -> Option<Adc> {
    let adc: Adc = serde_json::from_str(&std::fs::read_to_string(adc_path(state)).ok()?).ok()?;
    (adc.kind == "authorized_user").then_some(adc)
}

/// Google's token endpoint, or a test's own.
fn token_url() -> String {
    #[cfg(test)]
    if let Some(url) = test_token_url().lock().unwrap().clone() {
        return url;
    }
    "https://oauth2.googleapis.com/token".to_string()
}

#[cfg(test)]
fn test_token_url() -> &'static Mutex<Option<String>> {
    static URL: Mutex<Option<String>> = Mutex::new(None);
    &URL
}

/// An access token minted from the operator's gcloud sign-in, kept until a
/// few minutes before it expires: Google's real credential, which no room
/// ever holds.
fn google_token(state: &AppState) -> Option<String> {
    static CACHE: OnceLock<Mutex<HashMap<std::path::PathBuf, (String, std::time::Instant)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let key = state.base.to_path_buf();
    if let Some((token, until)) = cache.lock().unwrap().get(&key) {
        if std::time::Instant::now() < *until {
            return Some(token.clone());
        }
    }
    let adc = adc(state)?;
    let answer: serde_json::Value = ureq::post(&token_url())
        .timeout(std::time::Duration::from_secs(30))
        .send_form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &adc.client_id),
            ("client_secret", &adc.client_secret),
            ("refresh_token", &adc.refresh_token),
        ])
        .ok()?
        .into_json()
        .ok()?;
    let token = answer["access_token"].as_str()?.to_string();
    let life = answer["expires_in"].as_u64().unwrap_or(3600).saturating_sub(300);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(life);
    cache.lock().unwrap().insert(key, (token.clone(), until));
    Some(token)
}

/// The scope a stand-in token claims: Google Cloud's whole one, which the
/// operator's own sign-in holds.
const CLOUD_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// The GCE metadata server Google's tools find at their door
/// (`GCE_METADATA_HOST`), for a project that turned its Google sign-in on:
/// a stand-in token, which the door makes real on its way to Google, and the
/// project. Asked as Google's tools ask (`Metadata-Flavor: Google`).
pub(crate) async fn metadata(State(door): State<crate::doors::Door>, req: Request) -> Response {
    let (state, stream_id) = (door.state.clone(), door.stream_id.clone());
    let found = crate::actor::spawn_blocking(move || {
        let on = enabled(&state, &stream_id).iter().any(|s| s.name == "google");
        (on, on.then(|| adc(&state).and_then(|a| a.quota_project_id)).flatten())
    })
    .await
    .unwrap_or((false, None));
    let (on, project) = found;
    if !on {
        return StatusCode::NOT_FOUND.into_response();
    }
    let flavored = req
        .headers()
        .get("metadata-flavor")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"Google"));
    if !flavored {
        return (StatusCode::FORBIDDEN, "Missing Metadata-Flavor:Google header.").into_response();
    }
    let account = "/computeMetadata/v1/instance/service-accounts/default";
    let path = req.uri().path().trim_end_matches('/').to_string();
    let recursive = req.uri().query().is_some_and(|q| q.contains("recursive=true"));
    let (kind, body) = match path.as_str() {
        "" | "/computeMetadata/v1" => ("text/plain", "ok".to_string()),
        p if p == format!("{account}/token") => (
            "application/json",
            serde_json::json!({ "access_token": stand_in("google"), "expires_in": 3599, "token_type": "Bearer" }).to_string(),
        ),
        p if p == format!("{account}/email") => ("text/plain", "default".to_string()),
        p if p == format!("{account}/scopes") => ("text/plain", CLOUD_SCOPE.to_string()),
        p if p == account && recursive => (
            "application/json",
            serde_json::json!({ "aliases": ["default"], "email": "default", "scopes": [CLOUD_SCOPE] }).to_string(),
        ),
        "/computeMetadata/v1/instance/service-accounts" => ("text/plain", "default/\n".to_string()),
        "/computeMetadata/v1/project/project-id" => match project {
            Some(project) => ("text/plain", project),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    (
        [("metadata-flavor", "Google"), ("content-type", kind)],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn on(names: &[&str]) -> Vec<&'static SignIn> {
        SIGN_INS.iter().filter(|s| names.contains(&s.name)).collect()
    }

    /// Google, which takes no base URL: the metadata server at the door hands
    /// out a stand-in; a room's HTTPS to a Google host is answered by the
    /// door under its own authority, which the room trusts, and sent on with
    /// the stand-in made real; the real token is minted from the operator's
    /// own gcloud sign-in.
    /// The fake hosts a door sends to (`test_hosts`) are one map for the
    /// whole process: tests that point a host at their own server take turns.
    fn hosts_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn a_rooms_google_call_goes_through_its_door_with_the_token_made_real() {
        let _serial = hosts_test_lock();
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let base = dir.path().join(".bakery");
        std::fs::create_dir_all(&base).unwrap();
        let state = crate::testutil::state_with(&base, Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let stacks = crate::node::ProjectStacks {
            sign_ins: vec!["google".to_string()],
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();

        // The operator's gcloud sign-in, synthetic, and Google's token
        // endpoint, a test's own.
        let token_site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let token_at = token_site.local_addr().unwrap();
        *test_token_url().lock().unwrap() = Some(format!("http://127.0.0.1:{}/token", token_at.port()));
        std::thread::spawn(move || {
            for mut conn in token_site.incoming().flatten() {
                let mut asked = [0u8; 4096];
                let _ = conn.read(&mut asked);
                let body = r#"{"access_token":"real-google-token","expires_in":3600}"#;
                let answer = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = conn.write_all(answer.as_bytes());
            }
        });
        crate::testutil::first_connection(token_at);
        let gcloud = crate::machine_access::home_of(&state).join(".config/gcloud");
        std::fs::create_dir_all(&gcloud).unwrap();
        std::fs::write(
            gcloud.join("application_default_credentials.json"),
            r#"{"type":"authorized_user","client_id":"c","client_secret":"s","refresh_token":"r","quota_project_id":"farm-dev"}"#,
        )
        .unwrap();
        // A fresh binary's first loopback call can wait on LuLu, longer
        // under load: the first mint gets the long wait.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
        let minted = loop {
            match google_token(&state) {
                Some(token) => break token,
                None if std::time::Instant::now() < deadline => std::thread::sleep(std::time::Duration::from_millis(500)),
                None => panic!("the sign-in never minted a token"),
            }
        };
        assert_eq!(minted, "real-google-token", "minted from the sign-in");

        // Google's API host, a test's own.
        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        test_hosts().lock().unwrap().insert("storage.googleapis.com".to_string(), site.local_addr().unwrap().port());
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let _ = seen_tx.send(String::from_utf8_lossy(&head).into_owned());
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });

        let door = crate::doors::door(&state, &thread.stream_id).unwrap();
        crate::testutil::first_connection(([127, 0, 0, 1], door).into());
        let env = env_for(&state, &thread.stream_id, door);
        let value = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(value("GCE_METADATA_HOST"), Some(format!("127.0.0.1:{door}")));
        assert_eq!(value("SSL_CERT_FILE"), Some(crate::swap_tls::bundle(&base).display().to_string()));
        assert!(intercepts(&state, &thread.stream_id, "storage.googleapis.com:443"));
        assert!(!intercepts(&state, &thread.stream_id, "example.com:443"), "its sign-in hosts only");

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            // A fresh binary's first loopback call can wait on LuLu.
            let connect = || async {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                loop {
                    match tokio::net::TcpStream::connect(("127.0.0.1", door)).await {
                        Ok(tcp) => return tcp,
                        Err(e) => {
                            assert!(std::time::Instant::now() < deadline, "the door never answered: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        }
                    }
                }
            };
            let read_head = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();

            // The metadata server, as Google's tools ask it.
            let mut tcp = connect().await;
            tcp.write_all(b"GET /computeMetadata/v1/instance/service-accounts/default/token HTTP/1.1\r\nHost: x\r\nMetadata-Flavor: Google\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut answer = Vec::new();
            tcp.read_to_end(&mut answer).await.unwrap();
            let answer = read_head(&answer);
            assert!(answer.contains("200 OK") && answer.contains(&stand_in("google")), "{answer}");
            assert!(answer.to_ascii_lowercase().contains("metadata-flavor: google"), "{answer}");
            let mut tcp = connect().await;
            tcp.write_all(b"GET /computeMetadata/v1/project/project-id HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").await.unwrap();
            let mut answer = Vec::new();
            tcp.read_to_end(&mut answer).await.unwrap();
            assert!(read_head(&answer).starts_with("HTTP/1.1 403"), "only as Google's tools ask");

            // HTTPS to Google's host, through the door's own TLS: one
            // request each, its whole answer back.
            let base = base.as_path();
            let https = |request: String| async move {
                let mut tcp = connect().await;
                tcp.write_all(b"CONNECT storage.googleapis.com:443 HTTP/1.1\r\nHost: storage.googleapis.com:443\r\n\r\n").await.unwrap();
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && tcp.read(&mut byte).await.unwrap() == 1 {
                    head.push(byte[0]);
                }
                assert!(read_head(&head).starts_with("HTTP/1.1 200"), "{}", read_head(&head));
                let mut roots = rustls::RootCertStore::empty();
                roots.add(crate::swap_tls::ca(&base).unwrap().der()).unwrap();
                let client = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                    .with_safe_default_protocol_versions()
                    .unwrap()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                let name = rustls::pki_types::ServerName::try_from("storage.googleapis.com").unwrap();
                let mut tls = tokio_rustls::TlsConnector::from(Arc::new(client)).connect(name, tcp).await.unwrap();
                tls.write_all(request.as_bytes()).await.unwrap();
                let mut answer = Vec::new();
                let _ = tls.read_to_end(&mut answer).await;
                read_head(&answer)
            };
            let bearer = stand_in("google");
            let read = format!("GET /storage/v1/b?project=farm-dev HTTP/1.1\r\nHost: storage.googleapis.com\r\nAuthorization: Bearer {bearer}\r\nConnection: close\r\n\r\n");
            let answer = https(read).await;
            assert!(answer.starts_with("HTTP/1.1 200"), "a read goes through: {answer}");

            // A change to the operator's cloud with their sign-in waits on
            // their card, said the way Google's tools show it.
            let change = |method: &str, path: &str, body: &str| {
                format!("{method} {path} HTTP/1.1\r\nHost: storage.googleapis.com\r\nAuthorization: Bearer {bearer}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
            };
            let create = change("POST", "/storage/v1/b?project=farm-dev", r#"{"name":"farm-dev-new"}"#);
            let answer = https(create.clone()).await;
            assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
            assert!(answer.contains("waiting for the operator") && answer.contains("PERMISSION_DENIED"), "{answer}");
            let card = crate::remote::waiting(&state).items.into_iter().find(|w| w.stream_id == thread.stream_id).expect("its card");
            assert_eq!(card.tool, "google_change");

            // Approved, and the conversation may change farm-dev from now
            // on: that request goes, and another change there needs no card.
            let always = crate::pipe::ChildToolGateDecisionKind::AllowAlways;
            crate::pipe::decide_child_tool_gate(&state, &thread.stream_id, &card.card, always).unwrap();
            let answer = https(create).await;
            assert!(answer.starts_with("HTTP/1.1 200"), "approved, it went out: {answer}");
            let label = change("PATCH", "/storage/v1/b/farm-dev-new?project=farm-dev", r#"{"labels":{"app":"farm"}}"#);
            let answer = https(label).await;
            assert!(answer.starts_with("HTTP/1.1 200"), "granted, no card: {answer}");

            // A delete always asks.
            let delete = change("DELETE", "/storage/v1/b/farm-dev-new?project=farm-dev", "");
            let answer = https(delete).await;
            assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
        });
        let heads: Vec<String> = seen.try_iter().collect();
        let lines: Vec<&str> = heads.iter().map(|h| h.lines().next().unwrap_or_default()).collect();
        assert_eq!(
            lines,
            [
                "GET /storage/v1/b?project=farm-dev HTTP/1.1",
                "POST /storage/v1/b?project=farm-dev HTTP/1.1",
                "PATCH /storage/v1/b/farm-dev-new?project=farm-dev HTTP/1.1",
            ],
            "the read, the approved change and the granted one; never the delete"
        );
        assert!(heads.iter().all(|h| h.contains("Bearer real-google-token") && !h.contains(MARK)), "made real on its way: {heads:?}");
        let history = state.heart.lock().unwrap().events_of_family(&thread.stream_id, crate::remote::DONE).unwrap();
        let lines: Vec<Value> = history.iter().map(|e| serde_json::from_str(&e.payload).unwrap()).collect();
        let done: Vec<(&str, &str)> = lines.iter().map(|l| (l["method"].as_str().unwrap(), l["repository"].as_str().unwrap())).collect();
        assert_eq!(done, [("POST", "farm-dev"), ("PATCH", "farm-dev")], "each change that went out, in its history");
    }

    #[test]
    fn a_stand_in_becomes_real_only_for_its_own_hosts() {
        let real = |name: &str| Some(format!("real-{name}"));
        let both = on(&["anthropic", "openai"]);
        let key = stand_in("anthropic");
        assert_eq!(swapped(&key, "api.anthropic.com", &both, &real, &[]).unwrap(), "real-anthropic");
        let bearer = format!("Bearer {}", stand_in("openai"));
        assert_eq!(swapped(&bearer, "api.openai.com", &both, &real, &[]).unwrap(), "Bearer real-openai");
        let elsewhere = swapped(&key, "api.openai.com", &both, &real, &[]).unwrap_err();
        assert_eq!(elsewhere.0, StatusCode::FORBIDDEN, "never another host's");
        let off = swapped(&stand_in("xai"), "api.x.ai", &both, &real, &[]).unwrap_err();
        assert_eq!(off.0, StatusCode::FORBIDDEN, "a sign-in the project didn't turn on");
        assert_eq!(swapped("plain", "api.openai.com", &both, &real, &[]).unwrap(), "plain");
        let missing = swapped(&key, "api.anthropic.com", &both, &|_| None, &[]).unwrap_err();
        assert_eq!(missing.0, StatusCode::SERVICE_UNAVAILABLE);
    }

    /// A stand-in in a body is blanked, not refused: once a stand-in lands
    /// in the conversation (an agent ran `env`, read a file), every later
    /// request carries the whole conversation, so refusing would wedge the
    /// session. Each stand-in — a project's fixed one, or a thread's minted
    /// subscription one — becomes a filler of its own byte length, so the
    /// body's length and its Content-Length never change, no real credential
    /// is ever put in a body, and no stand-in leaves the Mac. A body with
    /// none is untouched. A body in any encoding but identity — whose bytes
    /// the door can't scan for a stand-in — is refused outright, with or
    /// without a mark in its raw bytes, as a header's leftover is.
    #[test]
    fn a_body_stand_in_is_blanked_to_the_same_length_and_a_compressed_body_refused() {
        let gemini = stand_in("gemini");
        let minted = format!("{}claude-native-0f1e2d3c4b5a6978", MARK); // synthetic, never a real token
        let body = format!("{{\"system\":\"my key is {gemini} and token {minted}\"}}");
        let out = blank_body(body.as_bytes(), &[]).unwrap().expect("a body with a stand-in is rewritten");
        assert_eq!(out.len(), body.len(), "length for length, so Content-Length holds");
        assert!(!out.windows(MARK.len()).any(|w| w == MARK.as_bytes()), "no stand-in survives");
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains(&gemini) && !out.contains(&minted), "{out}");
        // The structure around the stand-ins is left as it was.
        assert!(out.starts_with("{\"system\":\"my key is bakery-redacted-") && out.ends_with("\"}"), "{out}");

        // A body with no stand-in is handed back untouched.
        assert!(blank_body(b"{\"system\":\"nothing here\"}", &[]).unwrap().is_none());

        // A bare mark with nothing after it is still blanked (its own length).
        let bare = blank_body(MARK.as_bytes(), &[]).unwrap().unwrap();
        assert_eq!(bare.len(), MARK.len());
        assert!(!bare.windows(MARK.len()).any(|w| w == MARK.as_bytes()));

        // No header, or one or more identity lines, is fine; any other is
        // refused BEFORE the scan — even a body whose raw bytes show no mark
        // (a compressed one hiding a stand-in would look exactly like this),
        // so the encoding alone, not the mark, decides.
        assert!(blank_body(body.as_bytes(), &["identity"]).unwrap().is_some());
        assert!(blank_body(body.as_bytes(), &["identity", "identity"]).unwrap().is_some());
        assert!(blank_body(b"opaque compressed bytes, no mark here", &["gzip"]).unwrap_err().0 == StatusCode::FORBIDDEN, "a compressed body with no visible mark is still refused");
        // A single value, a comma-joined one, or a SECOND header line: any
        // non-identity anywhere refuses, so no second line slips gzip past.
        let refusing: &[&[&str]] = &[&["gzip"], &["br"], &["deflate"], &["zstd"], &["gzip, identity"], &["identity", "gzip"], &["identity, gzip"]];
        for encodings in refusing {
            let refused = blank_body(body.as_bytes(), encodings).unwrap_err();
            assert_eq!(refused.0, StatusCode::FORBIDDEN, "{encodings:?} is refused, not rewritten");
        }
    }

    /// A Content-Encoding line whose bytes aren't readable ASCII is read as
    /// the non-identity sentinel, not skipped, so the check never fails open:
    /// an unreadable line, alone or beside an identity line, refuses the body.
    #[test]
    fn an_unreadable_content_encoding_is_refused_not_skipped() {
        use axum::http::{header::CONTENT_ENCODING, HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        headers.append(CONTENT_ENCODING, HeaderValue::from_bytes(&[0xff, 0xfe]).unwrap());
        assert_eq!(request_encodings(&headers), ["?"], "an unreadable line reads as the non-identity sentinel");
        let body = format!("my key is {}", stand_in("gemini"));
        assert_eq!(blank_body(body.as_bytes(), &request_encodings(&headers)).unwrap_err().0, StatusCode::FORBIDDEN);
        // Even beside a readable identity line, the unreadable one still refuses.
        headers.append(CONTENT_ENCODING, HeaderValue::from_static("identity"));
        assert_eq!(blank_body(body.as_bytes(), &request_encodings(&headers)).unwrap_err().0, StatusCode::FORBIDDEN);
    }

    /// Through a thread's door: Gemini's key in its query made real, the
    /// request sent on to the host, its answer brought back; a stand-in in
    /// the body blanked out and the request still sent on; a host none of
    /// the project's sign-ins name, refused.
    #[test]
    fn a_door_sends_a_sign_in_hosts_request_on_with_its_stand_in_made_real() {
        let _serial = hosts_test_lock();
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        let stacks = crate::node::ProjectStacks {
            sign_ins: vec!["gemini".to_string()],
            ..Default::default()
        };
        crate::catalog::set_project_stacks(&state, &project.project_id, stacks).unwrap();
        plug("gemini", Arc::new(|_| Some("real-gemini-key".to_string())));

        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let site_port = site.local_addr().unwrap().port();
        test_hosts()
            .lock()
            .unwrap()
            .insert("generativelanguage.googleapis.com".to_string(), site_port);
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&head).into_owned();
                // The body too, by its Content-Length, so a blanked stand-in
                // can be read where the host would have seen it.
                let len = text
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|n| n.trim().parse::<usize>().ok()))
                    .unwrap_or(0);
                let mut body = vec![0u8; len];
                let _ = conn.read_exact(&mut body);
                let _ = seen_tx.send(text + &String::from_utf8_lossy(&body));
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });

        let door = crate::doors::door(&state, &thread.stream_id).unwrap();
        crate::testutil::first_connection(([127, 0, 0, 1], door).into());
        let env = env_for(&state, &thread.stream_id, door);
        let value = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap();
        assert_eq!(value("GEMINI_API_KEY"), stand_in("gemini"), "a stand-in, never the key");
        let base = value("GOOGLE_GEMINI_BASE_URL");
        assert_eq!(base, format!("http://127.0.0.1:{door}/to/generativelanguage.googleapis.com"));

        let call = |url: String, body: &str| {
            // A fresh binary's first loopback call can wait on LuLu.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            loop {
                let answer = ureq::post(&url)
                    .timeout(std::time::Duration::from_secs(120))
                    .send_string(body);
                match answer {
                    Ok(r) => return (r.status(), r.into_string().unwrap_or_default()),
                    Err(ureq::Error::Status(code, r)) => return (code, r.into_string().unwrap_or_default()),
                    Err(ureq::Error::Transport(t)) => {
                        assert!(std::time::Instant::now() < deadline, "the door never answered: {t}");
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        };
        let sent = call(format!("{base}/v1beta/models?key={}", stand_in("gemini")), "{}");
        assert_eq!(sent, (200, "ok".to_string()));
        let head = seen.recv_timeout(std::time::Duration::from_secs(120)).unwrap();
        assert!(head.starts_with("POST /v1beta/models?key=real-gemini-key HTTP/1.1"), "{head}");
        assert!(
            head.to_ascii_lowercase().contains("host: generativelanguage.googleapis.com"),
            "{head}"
        );

        // A stand-in in the body no longer refuses: it is blanked out, the
        // request goes on, and the host sees a filler of the same length
        // (22 bytes) — no stand-in, and Content-Length unchanged.
        let in_body = call(format!("{base}/v1beta/models"), &stand_in("gemini"));
        assert_eq!(in_body.0, 200, "a body's stand-in goes on, blanked: {}", in_body.1);
        let got = seen.recv_timeout(std::time::Duration::from_secs(120)).unwrap();
        assert!(got.starts_with("POST /v1beta/models HTTP/1.1"), "{got}");
        assert!(!got.contains(MARK), "the stand-in was blanked out of the body: {got}");
        assert!(got.to_ascii_lowercase().contains("content-length: 22"), "length held: {got}");
        // A host no sign-in names is still refused outright.
        let other = call(format!("http://127.0.0.1:{door}/to/api.openai.com/v1/models"), "{}");
        assert_eq!(other.0, 403, "{}", other.1);
        assert!(seen.try_recv().is_err(), "the unnamed host never heard from the door");
    }

    /// The grok CLI stand-in becomes the access token only toward the chat
    /// proxy. Cursor's host is not that proxy, and there is no Cursor row.
    #[test]
    fn a_grok_cli_stand_in_is_not_sent_to_cursor() {
        let grok = subscription_for("acp:grok", "subscription").unwrap();
        assert_eq!(grok.name(), "grok-cli");
        assert!(grok.serves(crate::grok_cli::CHAT_HOST));
        assert!(!grok.serves("api2.cursor.sh"));
        assert!(!grok.serves("api.x.ai"), "the HTTP API key is a different sign-in");
        assert!(subscription_for("cursor-agent", "subscription").is_none());
        assert!(subscription_for("agent", "subscription").is_none());
        assert!(SUBSCRIPTIONS.iter().all(|sub| !sub.serves("api2.cursor.sh")));
        let stand_in = "bakery-stand-in-grok-cli-abc";
        let header = format!("Bearer {stand_in}");
        assert!(swapped(&header, "api2.cursor.sh", &[], &|_| None, &[]).is_err());
        let toward_grok = swapped(
            &header,
            crate::grok_cli::CHAT_HOST,
            &[],
            &|_| None,
            &[(stand_in, "grok-fixture-access")],
        )
        .unwrap();
        assert_eq!(toward_grok, "Bearer grok-fixture-access");
        assert!(!grok.keep_env("XAI_API_KEY", "bakery-stand-in-xai"));
        assert!(!grok.keep_env("GROK_CODE_XAI_API_KEY", "anything"));
    }

    /// A subscription is not a project checkbox and not a fixed stand-in.
    /// The door makes the minted value real only for a thread whose seat is
    /// that subscription, and the value never enters the thread-wide env.
    #[test]
    fn a_subscription_stand_in_is_per_thread_and_only_that_seat_s_door_makes_it_real() {
        let _serial = hosts_test_lock();
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
        let (_project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
        std::fs::create_dir_all(crate::claude_sign_in::vault_dir(&state)).unwrap();
        std::fs::write(
            crate::claude_sign_in::vault_file(&state),
            r#"{"claudeAiOauth":{"accessToken":"real-access","refreshToken":"real-refresh","expiresAt":99999999999999}}"#,
        )
        .unwrap();

        let mine = minted(&state, "claude", &thread.stream_id);
        let someone_else = minted(&state, "claude", "wt:someone-else");
        assert_ne!(mine, someone_else);
        assert_ne!(mine, stand_in("claude"));

        let door = crate::doors::door(&state, &thread.stream_id).unwrap();
        crate::testutil::first_connection(([127, 0, 0, 1], door).into());
        let wide = env_for(&state, &thread.stream_id, door);
        assert!(
            wide.iter().all(|(key, _)| key != "CLAUDE_CODE_OAUTH_TOKEN"),
            "a subscription stand-in is not handed to every seat on the thread"
        );

        let call = |token: &str| {
            let url = format!("http://127.0.0.1:{door}/to/api.anthropic.com/v1/messages");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
            loop {
                let answer = ureq::post(&url)
                    .timeout(std::time::Duration::from_secs(120))
                    .set("Authorization", &format!("Bearer {token}"))
                    .send_string("{}");
                match answer {
                    Ok(response) => return (response.status(), response.into_string().unwrap_or_default()),
                    Err(ureq::Error::Status(code, response)) => {
                        return (code, response.into_string().unwrap_or_default())
                    }
                    Err(ureq::Error::Transport(transport)) => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the door never answered: {transport}"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                }
            }
        };

        let closed = call(&mine);
        assert_eq!(closed.0, 403, "{}", closed.1);

        crate::routes::seats::seat_assign_checked(
            &state,
            &thread.stream_id,
            crate::node::Seat::Orchestrator,
            crate::claude_native::ACCOUNT_ID,
        )
        .unwrap();
        let seat_env = subscription_env(&state, &thread.stream_id, crate::claude_native::ACCOUNT_ID, "subscription", door);
        assert_eq!(
            seat_env
                .iter()
                .find(|(key, _)| key == "CLAUDE_CODE_OAUTH_TOKEN")
                .map(|(_, value)| value.as_str()),
            Some(mine.as_str())
        );

        let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let site_port = site.local_addr().unwrap().port();
        test_hosts().lock().unwrap().insert("api.anthropic.com".to_string(), site_port);
        let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for mut conn in site.incoming().flatten() {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                    head.push(byte[0]);
                }
                let _ = seen_tx.send(String::from_utf8_lossy(&head).into_owned());
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
            }
        });

        let wrong = call(&someone_else);
        assert_eq!(wrong.0, 403, "{}", wrong.1);
        assert!(seen.try_recv().is_err(), "the wrong stand-in never left the door");

        let sent = call(&mine);
        assert_eq!(sent.0, 200, "{}", sent.1);
        let head = seen.recv_timeout(std::time::Duration::from_secs(120)).unwrap();
        assert!(
            head.to_ascii_lowercase().contains("authorization: bearer real-access"),
            "{head}"
        );
        assert!(!head.contains(&mine), "{head}");
        assert!(!head.contains("real-refresh"), "{head}");
    }

    /// Claude native's console and OpenRouter providers, end to end through
    /// a thread's real door: each mints its OWN per-thread stand-in, never
    /// the operator's saved key, in the right env var; the door makes that
    /// stand-in real ONLY toward that provider's own host — a request
    /// carrying the WRONG provider's stand-in to that host is refused and
    /// the host never hears from it; and switching the seat from one
    /// provider straight to the other needs nothing but the assignment
    /// (no login step at all — each provider is a saved key, read fresh).
    #[test]
    fn claude_native_providers_mint_per_thread_and_the_door_swaps_only_toward_their_own_host() {
        let _serial = hosts_test_lock();
        use std::io::{Read, Write};
        let secrets_dir = tempfile::tempdir().unwrap();
        bakery_agent::secrets::test_support::with_isolated_credentials(&secrets_dir.path().join("secrets.json"), || {
            // Synthetic: never real keys.
            bakery_agent::secrets::set_api_key("anthropic", "real-console-key").unwrap();
            bakery_agent::secrets::set_api_key("openrouter", "real-openrouter-key").unwrap();

            let dir = tempfile::tempdir().unwrap();
            let work = tempfile::tempdir().unwrap();
            let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
            let root = crate::catalog::DirectoryRootSpec::Attach(work.path().to_str().unwrap());
            let (_project, thread) = crate::catalog::create_project(&state, "farm", Some(root)).unwrap();
            let door = crate::doors::door(&state, &thread.stream_id).unwrap();
            crate::testutil::first_connection(([127, 0, 0, 1], door).into());

            let call = |path_and_host: &str, header: &str, token: &str| {
                let url = format!("http://127.0.0.1:{door}/to/{path_and_host}");
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                loop {
                    let answer = ureq::post(&url)
                        .timeout(std::time::Duration::from_secs(120))
                        .set(header, token)
                        .send_string("{}");
                    match answer {
                        Ok(response) => return (response.status(), response.into_string().unwrap_or_default()),
                        Err(ureq::Error::Status(code, response)) => return (code, response.into_string().unwrap_or_default()),
                        Err(ureq::Error::Transport(transport)) => {
                            assert!(std::time::Instant::now() < deadline, "the door never answered: {transport}");
                            std::thread::sleep(std::time::Duration::from_millis(200));
                        }
                    }
                }
            };
            let stand_in_host = |host: &str| -> (std::sync::mpsc::Receiver<String>, u16) {
                let site = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
                let port = site.local_addr().unwrap().port();
                test_hosts().lock().unwrap().insert(host.to_string(), port);
                let (seen_tx, seen) = std::sync::mpsc::channel::<String>();
                std::thread::spawn(move || {
                    for mut conn in site.incoming().flatten() {
                        let mut head = Vec::new();
                        let mut byte = [0u8; 1];
                        while !head.ends_with(b"\r\n\r\n") && conn.read(&mut byte).unwrap_or(0) == 1 {
                            head.push(byte[0]);
                        }
                        let _ = seen_tx.send(String::from_utf8_lossy(&head).into_owned());
                        let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
                    }
                });
                (seen, port)
            };
            let (anthropic_seen, _) = stand_in_host("api.anthropic.com");
            let (openrouter_seen, _) = stand_in_host("openrouter.ai");

            // --- console, first ---
            crate::state::assign_seat_model_for(
                &state,
                &thread.stream_id,
                crate::node::Seat::Orchestrator,
                crate::claude_native::ACCOUNT_ID,
                None,
                None,
                Some("console"),
            )
            .unwrap();
            let console_stand_in = minted(&state, "claude-console", &thread.stream_id);
            let env = subscription_env(&state, &thread.stream_id, crate::claude_native::ACCOUNT_ID, "console", door);
            assert_eq!(value(&env, "ANTHROPIC_API_KEY").as_deref(), Some(console_stand_in.as_str()), "the stand-in, never the real key, in the seat's own env");
            assert_eq!(value(&env, "ANTHROPIC_BASE_URL").as_deref(), Some(format!("http://127.0.0.1:{door}/to/api.anthropic.com").as_str()));

            // The console stand-in reaches api.anthropic.com as the real key…
            let ok = call("api.anthropic.com/v1/messages", "x-api-key", &console_stand_in);
            assert_eq!(ok.0, 200, "{}", ok.1);
            let head = anthropic_seen.recv_timeout(std::time::Duration::from_secs(120)).unwrap();
            assert!(head.to_ascii_lowercase().contains("x-api-key: real-console-key"), "{head}");
            assert!(!head.contains(&console_stand_in), "{head}");
            // …but carries no key at all toward a host that isn't its own:
            // refused at the door, and the OpenRouter host never hears from it.
            let elsewhere = call("openrouter.ai/api/v1/messages", "Authorization", &format!("Bearer {console_stand_in}"));
            assert_eq!(elsewhere.0, 403, "{}", elsewhere.1);
            assert!(openrouter_seen.try_recv().is_err(), "the console stand-in never reached openrouter.ai");

            // --- switch straight to openrouter: no login step, just the assignment ---
            crate::state::assign_seat_model_for(
                &state,
                &thread.stream_id,
                crate::node::Seat::Orchestrator,
                crate::claude_native::ACCOUNT_ID,
                None,
                None,
                Some("openrouter"),
            )
            .unwrap();
            let openrouter_stand_in = minted(&state, "claude-openrouter", &thread.stream_id);
            let env = subscription_env(&state, &thread.stream_id, crate::claude_native::ACCOUNT_ID, "openrouter", door);
            assert_eq!(value(&env, "ANTHROPIC_AUTH_TOKEN").as_deref(), Some(openrouter_stand_in.as_str()));
            assert_eq!(value(&env, "ANTHROPIC_BASE_URL").as_deref(), Some(format!("http://127.0.0.1:{door}/to/openrouter.ai/api").as_str()));
            assert_eq!(value(&env, "ANTHROPIC_API_KEY").as_deref(), Some(""), "blanked, so Claude Code never falls back to it");

            let ok = call("openrouter.ai/api/v1/messages", "Authorization", &format!("Bearer {openrouter_stand_in}"));
            assert_eq!(ok.0, 200, "{}", ok.1);
            let head = openrouter_seen.recv_timeout(std::time::Duration::from_secs(120)).unwrap();
            assert!(head.to_ascii_lowercase().contains("authorization: bearer real-openrouter-key"), "{head}");
            assert!(!head.contains(&openrouter_stand_in), "{head}");
            // The OLD console stand-in is dead now too — the thread carries
            // the one provider at a time, so a leftover console stand-in
            // reaching api.anthropic.com is refused, not swapped.
            let stale = call("api.anthropic.com/v1/messages", "x-api-key", &console_stand_in);
            assert_eq!(stale.0, 403, "{}", stale.1);
        });
    }

    fn value(env: &[(String, String)], key: &str) -> Option<String> {
        env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn a_subscription_seed_is_persisted_and_profile_replies_lose_identity() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = crate::testutil::state_with(dir.path(), Arc::new(bakery_agent::StubModel));
        let first = minted(&state, "claude", "wt:a");
        assert_eq!(first, minted(&state, "claude", "wt:a"));
        assert_ne!(first, minted(&state, "claude", "wt:b"));
        let seed = state.base.join("swap").join("subscription-seed");
        assert_eq!(std::fs::read(&seed).unwrap().len(), 32);
        assert_eq!(std::fs::metadata(&seed).unwrap().permissions().mode() & 0o777, 0o600);

        assert!(identity_path("api.anthropic.com", "/api/oauth/profile"));
        assert!(identity_path("api.anthropic.com", "/api/oauth/account/settings"));
        assert!(!identity_path("api.anthropic.com", "/v1/messages"));
        assert!(!identity_path("api.openai.com", "/api/oauth/profile"));

        let mut profile = json!({
            "account": {
                "email": "person@example.invalid",
                "subscriptionType": "max",
                "accountUuid": "account-secret"
            },
            "models": [{"name": "opus", "id": "claude-opus"}]
        });
        strip_identity_keys(&mut profile);
        let text = profile.to_string();
        assert!(!text.contains("person@example.invalid"), "{text}");
        assert!(!text.contains("account-secret"), "{text}");
        assert!(text.contains("max"), "{text}");
        assert!(text.contains("opus"), "{text}");
    }
}
