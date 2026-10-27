//! Public seam for operator-local Claude Code login.
//!
//! The OAuth client itself is not in this repository. When it is present on
//! the node, these functions start/complete a browser login. MFA happens in
//! Anthropic's browser. Tokens land in `secrets.json`. `auto` never selects
//! this id.

use crate::{Model, ModelCatalog, ModelCatalogEntry};
use serde::Serialize;

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct LoginStart {
    pub verification_uri: String,
    pub built: bool,
}

#[derive(schemars::JsonSchema, Debug, Clone, Serialize)]
pub struct LoginResult {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn is_built() -> bool {
    cfg!(has_claude_code_oauth) || test_provider_enabled()
}

pub fn credentials_available() -> bool {
    if test_provider_enabled() {
        return crate::secrets::load()
            .anthropic_oauth
            .is_some_and(|tokens| !tokens.access.is_empty() || !tokens.refresh.is_empty());
    }
    #[cfg(has_claude_code_oauth)]
    {
        return crate::claude_code_oauth::ClaudeCodeModel::credentials_available();
    }
    #[cfg(not(has_claude_code_oauth))]
    false
}

/// Ask the installed Claude Code account adapter for the model catalog it
/// actually exposes. The private provider remains behind this public seam:
/// callers receive model metadata only, never OAuth material. Older/private
/// adapters that have not implemented `Model::catalog()` retain the trait's
/// explicit `Unavailable` response instead of acquiring a guessed table.
pub fn catalog() -> Result<ModelCatalog, String> {
    if test_provider_enabled() {
        return Ok(unavailable_catalog(
            "the test Claude Code provider does not report a live catalog",
        ));
    }
    #[cfg(has_claude_code_oauth)]
    {
        if !credentials_available() {
            return Ok(unavailable_catalog(
                "Setup needed: sign in with Claude Code before listing models",
            ));
        }
        return crate::claude_code_oauth::ClaudeCodeModel::from_env().catalog();
    }
    #[cfg(not(has_claude_code_oauth))]
    Ok(unavailable_catalog(
        "this model does not report a live catalog",
    ))
}

fn unavailable_catalog(reason: &str) -> ModelCatalog {
    ModelCatalog::Unavailable {
        default: ModelCatalogEntry {
            id: "claude-code".to_string(),
            label: "Claude Code".to_string(),
            context_window: None,
            effort_levels: Vec::new(),
        },
        reason: reason.to_string(),
    }
}

pub fn login_start() -> Result<LoginStart, String> {
    if test_provider_enabled() {
        return Ok(LoginStart {
            verification_uri: "https://claude.ai/test-only-login".into(),
            built: true,
        });
    }
    #[cfg(has_claude_code_oauth)]
    {
        return crate::claude_code_oauth::login_start();
    }
    #[cfg(not(has_claude_code_oauth))]
    Err("Setup needed: Claude Code login is not built on this node".into())
}

pub fn login_complete(callback: &str) -> Result<LoginResult, String> {
    if test_provider_enabled() {
        if callback != "test-only-callback" {
            return Err("Claude Code test login was not completed".into());
        }
        crate::secrets::set_anthropic_oauth(crate::secrets::OAuthTokens {
            access: "test-only-claude-access".into(),
            refresh: "test-only-claude-refresh".into(),
            expires: 1,
            email: None,
            project: None,
        })?;
        return Ok(LoginResult {
            status: "complete".into(),
            error: None,
        });
    }
    #[cfg(has_claude_code_oauth)]
    {
        return crate::claude_code_oauth::login_complete(callback);
    }
    #[cfg(not(has_claude_code_oauth))]
    {
        let _ = callback;
        Err("Setup needed: Claude Code login is not built on this node".into())
    }
}

/// Test-only switch for exercising the public account routes without loading
/// the operator's private provider source or touching any live credential
/// surface. The `test-util` feature is enabled only for crate tests and
/// bakery-node's dev dependency.
fn test_provider_enabled() -> bool {
    #[cfg(any(test, feature = "test-util"))]
    {
        return std::env::var("BAKERY_CLAUDE_CODE_PROVIDER_FOR_TEST").as_deref() == Ok("1");
    }
    #[cfg(not(any(test, feature = "test-util")))]
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_login_seam_round_trips_without_exposing_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        crate::secrets::test_support::with_isolated_credentials(&path, || {
            std::env::set_var("BAKERY_CLAUDE_CODE_PROVIDER_FOR_TEST", "1");

            assert!(is_built());
            assert!(!credentials_available());
            let start = login_start().unwrap();
            assert_eq!(start.verification_uri, "https://claude.ai/test-only-login");
            let done = login_complete("test-only-callback").unwrap();
            assert_eq!(done.status, "complete");
            assert!(credentials_available());

            let public = serde_json::to_string(&crate::secrets::status()).unwrap();
            assert!(!public.contains("test-only-claude-access"));
            assert!(!public.contains("test-only-claude-refresh"));
        });
    }
}
