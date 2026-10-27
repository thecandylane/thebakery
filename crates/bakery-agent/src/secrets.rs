//! Local secrets store — never written into the Heart mutation log.
//!
//! Path: `$HOME/.bakery/secrets.json` (mode 0600). Holds API keys and OAuth
//! tokens for model providers. The thin client POSTs credentials here via the
//! node; the node is the only process that should read this file.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access: String,
    pub refresh: String,
    /// Epoch milliseconds (0 = unknown).
    #[serde(default)]
    pub expires: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Gemini Code Assist project id when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecretsFile {
    /// Preferred model id for resolve: stub | grok | gemini | claude | auto | claude-code
    #[serde(default)]
    pub active_model: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xai_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_api_key: Option<String>,
    /// OpenRouter's own key — a native Claude seat set to the openrouter
    /// provider sends it as a bearer through the door, toward
    /// `openrouter.ai` only; never a Bakery HTTP model's own credential.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter_api_key: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xai_oauth: Option<OAuthTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_oauth: Option<OAuthTokens>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_oauth: Option<OAuthTokens>,

    /// OAuth client secret for the Gemini desktop client. NOT shipped in the
    /// source tree — see `gemini::client_secret()`. Supplied by the operator
    /// (env or this file) so no credential material ever lives in the repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_client_secret: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grok_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_model_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_model_id: Option<String>,

    /// Bearer tokens for operator-configured openai-compat plugs
    /// (`node.json`'s `openai_compat_plugs[].id` → token), keyed by plug id
    /// rather than a fixed field per provider — an openai-compat plug is a
    /// named local/self-hosted endpoint, not one of the three closed HTTP
    /// providers above. `None`/absent means no bearer (a local server with
    /// no auth, e.g. bare Ollama, is a normal configuration).
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub openai_compat_secrets: std::collections::HashMap<String, String>,
}

/// Public status only — never includes secret material.
#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct AuthStatus {
    pub active_model: String,
    pub resolved_hint: String,
    pub xai: ProviderStatus,
    pub gemini: ProviderStatus,
    pub anthropic: ProviderStatus,
    pub openrouter: ProviderStatus,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct ProviderStatus {
    pub api_key_set: bool,
    pub oauth_set: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

pub fn secrets_path() -> PathBuf {
    // Test-only escape hatch (`test-util` feature, off in every normal and
    // release build) so tests can isolate credential state instead of
    // reading/writing the operator's real `$HOME/.bakery/secrets.json`
    // (06b-PIPE.md: "Do not: Live secrets.json in tests").
    #[cfg(any(test, feature = "test-util"))]
    if let Ok(p) = std::env::var("BAKERY_SECRETS_PATH_FOR_TEST") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".bakery/secrets.json")
}

pub fn load() -> SecretsFile {
    let path = secrets_path();
    load_from(&path).unwrap_or_default()
}

pub fn load_from(path: &Path) -> Result<SecretsFile, String> {
    if !path.exists() {
        return Ok(SecretsFile::default());
    }
    let raw = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&raw).map_err(|e| format!("secrets parse: {e}"))
}

pub fn save(s: &SecretsFile) -> Result<(), String> {
    save_to(&secrets_path(), s)
}

pub fn save_to(path: &Path, s: &SecretsFile) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    fs::write(path, raw).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn status() -> AuthStatus {
    status_of(&load())
}

/// Build public status from an in-memory secrets file (and process env for key presence).
/// Used by `/auth/status` and tests — must never embed secret material in the result.
pub fn status_of(s: &SecretsFile) -> AuthStatus {
    let active = s
        .active_model
        .clone()
        .or_else(|| std::env::var("BAKERY_MODEL").ok())
        .unwrap_or_else(|| "auto".into());
    AuthStatus {
        active_model: active.clone(),
        resolved_hint: resolve_hint(s, &active),
        xai: ProviderStatus {
            api_key_set: non_empty(s.xai_api_key.as_deref())
                || non_empty(std::env::var("XAI_API_KEY").ok().as_deref()),
            oauth_set: s.xai_oauth.as_ref().is_some_and(|o| !o.access.is_empty()),
            oauth_email: s.xai_oauth.as_ref().and_then(|o| o.email.clone()),
            model_id: s.grok_model_id.clone(),
        },
        gemini: ProviderStatus {
            api_key_set: non_empty(s.gemini_api_key.as_deref())
                || non_empty(std::env::var("GEMINI_API_KEY").ok().as_deref())
                || non_empty(std::env::var("GOOGLE_API_KEY").ok().as_deref()),
            oauth_set: s
                .gemini_oauth
                .as_ref()
                .is_some_and(|o| !o.access.is_empty() || !o.refresh.is_empty()),
            oauth_email: s.gemini_oauth.as_ref().and_then(|o| o.email.clone()),
            model_id: s.gemini_model_id.clone(),
        },
        anthropic: ProviderStatus {
            api_key_set: non_empty(s.anthropic_api_key.as_deref())
                || non_empty(std::env::var("ANTHROPIC_API_KEY").ok().as_deref()),
            oauth_set: s
                .anthropic_oauth
                .as_ref()
                .is_some_and(|o| !o.access.is_empty() || !o.refresh.is_empty()),
            oauth_email: None,
            model_id: s.claude_model_id.clone(),
        },
        // Settings' own place for it — a native Claude seat's openrouter
        // provider reads this, never an OAuth sign-in of its own.
        openrouter: ProviderStatus {
            api_key_set: non_empty(s.openrouter_api_key.as_deref()),
            oauth_set: false,
            oauth_email: None,
            model_id: None,
        },
    }
}

fn non_empty(s: Option<&str>) -> bool {
    s.is_some_and(|v| !v.trim().is_empty())
}

fn resolve_hint(s: &SecretsFile, active: &str) -> String {
    match active.to_ascii_lowercase().as_str() {
        "stub" => "stub".into(),
        "grok" | "xai" => "grok".into(),
        "gemini" => "gemini".into(),
        "claude" => "claude".into(),
        "claude-code" | "claude_code" => "claude-code".into(),
        _ => {
            if s.xai_oauth.is_some() || non_empty(s.xai_api_key.as_deref()) {
                "auto→grok".into()
            } else if s.gemini_oauth.is_some() || non_empty(s.gemini_api_key.as_deref()) {
                "auto→gemini".into()
            } else if non_empty(s.anthropic_api_key.as_deref()) {
                "auto→claude".into()
            } else {
                "auto→stub".into()
            }
        }
    }
}

pub fn set_api_key(provider: &str, key: &str) -> Result<(), String> {
    let mut s = load();
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("key cannot be empty".into());
    }
    match provider.to_ascii_lowercase().as_str() {
        "xai" | "grok" => s.xai_api_key = Some(key),
        "gemini" | "google" => s.gemini_api_key = Some(key),
        "anthropic" | "claude" => s.anthropic_api_key = Some(key),
        "openrouter" => s.openrouter_api_key = Some(key),
        other => return Err(format!("unknown provider {other}")),
    }
    save(&s)
}

pub fn clear_provider(provider: &str) -> Result<(), String> {
    let mut s = load();
    match provider.to_ascii_lowercase().as_str() {
        "xai" | "grok" => {
            s.xai_api_key = None;
            s.xai_oauth = None;
        }
        "gemini" | "google" => {
            s.gemini_api_key = None;
            s.gemini_oauth = None;
        }
        "anthropic" | "claude" => {
            s.anthropic_api_key = None;
            s.anthropic_oauth = None;
        }
        "claude-code" | "claude_code" => s.anthropic_oauth = None,
        "openrouter" => s.openrouter_api_key = None,
        other => return Err(format!("unknown provider {other}")),
    }
    save(&s)
}

/// Set (or replace) the bearer token for one openai-compat plug id. The
/// caller (bakery-node's `/auth/plug-secret`) validates the plug id is one
/// this node has configured; this store itself is plug-list-agnostic, the
/// same way it never validates `provider` strings against a live list.
pub fn set_plug_secret(plug_id: &str, key: &str) -> Result<(), String> {
    let plug_id = plug_id.trim();
    if plug_id.is_empty() {
        return Err("plug_id cannot be empty".into());
    }
    let key = key.trim().to_string();
    if key.is_empty() {
        return Err("key cannot be empty".into());
    }
    let mut s = load();
    s.openai_compat_secrets.insert(plug_id.to_string(), key);
    save(&s)
}

pub fn clear_plug_secret(plug_id: &str) -> Result<(), String> {
    let mut s = load();
    s.openai_compat_secrets.remove(plug_id.trim());
    save(&s)
}

/// The live bearer token for one openai-compat plug, if any — read by the
/// resolver when constructing an `OpenAiCompatModel`. Never surfaced over
/// HTTP; only [`plug_secret_set`]'s boolean is.
pub fn plug_secret(plug_id: &str) -> Option<String> {
    load().openai_compat_secrets.get(plug_id.trim()).cloned()
}

/// Boolean-only presence check for one plug id — what `/studio/state` and
/// Settings → Providers are allowed to know about an openai-compat bearer.
pub fn plug_secret_set(plug_id: &str) -> bool {
    non_empty(load().openai_compat_secrets.get(plug_id.trim()).map(|s| s.as_str()))
}

pub fn set_active_model(model: &str) -> Result<(), String> {
    let mut s = load();
    s.active_model = Some(model.trim().to_ascii_lowercase());
    save(&s)
}

pub fn set_xai_oauth(tokens: OAuthTokens) -> Result<(), String> {
    let mut s = load();
    s.xai_oauth = Some(tokens);
    if s.active_model.is_none() {
        s.active_model = Some("grok".into());
    }
    save(&s)
}

pub fn set_gemini_oauth(tokens: OAuthTokens) -> Result<(), String> {
    let mut s = load();
    s.gemini_oauth = Some(tokens);
    if s.active_model.is_none() {
        s.active_model = Some("gemini".into());
    }
    save(&s)
}

pub fn set_anthropic_oauth(tokens: OAuthTokens) -> Result<(), String> {
    let mut s = load();
    s.anthropic_oauth = Some(tokens);
    save(&s)
}

/// Test-only isolation for credential state, off in every normal and release
/// build (see the `test-util` feature doc comment in `Cargo.toml`). Every
/// `credentials_available()` also reads a provider env var directly
/// (`claude.rs`/`grok.rs`/`gemini.rs`), so isolating just [`secrets_path`]
/// is not enough for a test to prove "no credentials configured" — this
/// scrubs both together, under one lock, so concurrent tests can't observe
/// each other's temporary override (06b-PIPE.md: "Do not: Live secrets.json
/// in tests").
#[cfg(any(test, feature = "test-util"))]
pub mod test_support {
    use std::sync::Mutex;

    pub static CREDENTIAL_ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Every env var a `credentials_available()` implementation reads
    /// directly, bypassing `secrets::load()` entirely — plus the ACP
    /// process-override vars (`bakery-node`'s `resolve_acp_binary_config`),
    /// since those are just as capable of pulling live/ambient config into a
    /// test as a provider API key is.
    pub const PROVIDER_ENV_VARS: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "XAI_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "BAKERY_GEMINI_CLIENT_SECRET",
        "BAKERY_ACP_CLI",
        "BAKERY_ACP_ARGS",
        "BAKERY_ACP_CONFIG_SRC",
        "BAKERY_ACP_CONFIG_DEST",
        "BAKERY_CLAUDE_CODE_PROVIDER_FOR_TEST",
    ];

    /// Runs `f` with `BAKERY_SECRETS_PATH_FOR_TEST` pointed at `secrets_path`
    /// (a caller-owned tempdir path — this module deliberately takes no
    /// dependency on `tempfile` itself) and every [`PROVIDER_ENV_VARS`] entry
    /// cleared, restoring both afterward. Holds [`CREDENTIAL_ENV_LOCK`] for
    /// the whole call — callers must not nest two calls on the same thread.
    pub fn with_isolated_credentials<T>(secrets_path: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let guard = CREDENTIAL_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_path = std::env::var("BAKERY_SECRETS_PATH_FOR_TEST").ok();
        std::env::set_var("BAKERY_SECRETS_PATH_FOR_TEST", secrets_path);
        let prev_vars: Vec<Option<String>> = PROVIDER_ENV_VARS.iter().map(|k| std::env::var(k).ok()).collect();
        for k in PROVIDER_ENV_VARS {
            std::env::remove_var(k);
        }

        let result = f();

        for (k, v) in PROVIDER_ENV_VARS.iter().zip(prev_vars) {
            match v {
                Some(v) => std::env::set_var(k, v),
                None => std::env::remove_var(k),
            }
        }
        match prev_path {
            Some(v) => std::env::set_var("BAKERY_SECRETS_PATH_FOR_TEST", v),
            None => std::env::remove_var("BAKERY_SECRETS_PATH_FOR_TEST"),
        }
        drop(guard);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_secrets_no_leak_in_status() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let mut s = SecretsFile::default();
        // Deliberately not real provider formats (no GOCSPX-/sk-/ya29. prefixes)
        // so public-repo secret scanners do not false-positive on unit tests.
        let api_key = "test-only-api-key-not-a-real-credential";
        let access = "test-only-access-token-not-a-real-credential";
        let refresh = "test-only-refresh-token-not-a-real-credential";
        let client_secret = "test-only-oauth-client-secret-not-a-real-credential";
        s.xai_api_key = Some(api_key.into());
        s.gemini_client_secret = Some(client_secret.into());
        s.xai_oauth = Some(OAuthTokens {
            access: access.into(),
            refresh: refresh.into(),
            expires: 99,
            email: Some("u@x.ai".into()),
            project: None,
        });
        save_to(&path, &s).unwrap();
        let mut loaded = load_from(&path).unwrap();
        assert_eq!(loaded.xai_api_key.as_deref(), Some(api_key));

        let st = status_of(&loaded);
        assert!(st.xai.api_key_set);
        assert!(st.xai.oauth_set);
        assert_eq!(st.xai.oauth_email.as_deref(), Some("u@x.ai"));

        loaded.anthropic_oauth = Some(OAuthTokens {
            access: access.into(),
            refresh: refresh.into(),
            expires: 99,
            email: Some("must-not-leak@example.com".into()),
            project: None,
        });
        let st = status_of(&loaded);
        assert!(st.anthropic.oauth_set);
        assert!(st.anthropic.oauth_email.is_none(), "Claude Code login must not expose email");

        let json = serde_json::to_string(&st).expect("AuthStatus serializes");
        for forbidden in [api_key, access, refresh, client_secret] {
            assert!(
                !json.contains(forbidden),
                "AuthStatus JSON must not contain secret material: leaked {forbidden:?} in {json}"
            );
        }
    }

    #[test]
    fn openrouter_key_saves_clears_and_never_leaks_into_status() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        // Synthetic: never a real OpenRouter key.
        let key = "test-only-openrouter-key-not-a-real-credential";
        test_support::with_isolated_credentials(&path, || {
            assert!(!status().openrouter.api_key_set);

            set_api_key("openrouter", key).unwrap();
            let s = load();
            assert_eq!(s.openrouter_api_key.as_deref(), Some(key));
            let st = status_of(&s);
            assert!(st.openrouter.api_key_set);
            assert!(!serde_json::to_string(&st).unwrap().contains(key), "the key itself must not leak into status");

            clear_provider("openrouter").unwrap();
            assert!(!status().openrouter.api_key_set);
            assert_eq!(load().openrouter_api_key, None);
        });
    }

    #[test]
    fn plug_secret_roundtrips_and_only_the_boolean_is_exposed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        test_support::with_isolated_credentials(&path, || {
            assert!(!plug_secret_set("local:ollama"));
            assert_eq!(plug_secret("local:ollama"), None);

            set_plug_secret("local:ollama", "test-only-bearer-not-a-real-credential").unwrap();
            assert!(plug_secret_set("local:ollama"));
            assert_eq!(plug_secret("local:ollama").as_deref(), Some("test-only-bearer-not-a-real-credential"));
            // A different, unconfigured plug id must not see this one's secret.
            assert!(!plug_secret_set("local:other"));

            clear_plug_secret("local:ollama").unwrap();
            assert!(!plug_secret_set("local:ollama"));
            assert_eq!(plug_secret("local:ollama"), None);
        });
    }

    #[test]
    fn set_plug_secret_refuses_empty_id_or_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        test_support::with_isolated_credentials(&path, || {
            assert!(set_plug_secret("", "key").is_err());
            assert!(set_plug_secret("local:ollama", "   ").is_err());
        });
    }
}
