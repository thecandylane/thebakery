//! The model proxy route. When the node's model proxy is running and holds a
//! key for a provider, that provider's API-key calls go to it, at the same
//! path, with the node's own client token where a key would be. Otherwise,
//! or once it stops, each plug calls its provider directly, as before.

use std::cell::RefCell;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Where API-key calls go while the model proxy serves them.
pub struct Route {
    /// The proxy's base URL, e.g. `http://127.0.0.1:47791`.
    pub base: String,
    /// The node's client token for it.
    pub token: String,
    /// The providers it holds a key for: `claude`, `gemini`, `xai`.
    pub providers: Vec<String>,
}

static ROUTE: RwLock<Option<Route>> = RwLock::new(None);

/// The proxy's model ids as last listed, for one base URL.
static LISTED: Mutex<Option<(String, Instant, Vec<String>)>> = Mutex::new(None);

thread_local! {
    /// The calling agent's own token, for the turn running on this thread.
    static AGENT_TOKEN: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Calls made on this thread carry `token` (an agent's own) instead of the
/// node's until the returned scope ends, so their usage lands on that
/// agent. `None` changes nothing.
pub fn as_agent(token: Option<String>) -> AgentScope {
    AgentScope(AGENT_TOKEN.with(|t| t.replace(token)))
}

/// Restores the thread's previous caller when dropped.
pub struct AgentScope(Option<String>);

impl Drop for AgentScope {
    fn drop(&mut self) {
        AGENT_TOKEN.with(|t| *t.borrow_mut() = self.0.take());
    }
}

/// Set, or clear, where API-key calls go. The node calls this.
pub fn set(route: Option<Route>) {
    *ROUTE.write().unwrap_or_else(|e| e.into_inner()) = route;
    *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The proxy's base URL and token, when it serves `provider`.
pub fn route(provider: &str) -> Option<(String, String)> {
    let route = ROUTE.read().unwrap_or_else(|e| e.into_inner());
    let route = route.as_ref()?;
    let token = AGENT_TOKEN
        .with(|t| t.borrow().clone())
        .unwrap_or_else(|| route.token.clone());
    route
        .providers
        .iter()
        .any(|p| p == provider)
        .then(|| (route.base.clone(), token))
}

/// `direct` (a provider's own API URL) on the proxy, with the token to send
/// in place of a key, when the proxy serves `provider`.
pub fn via(provider: &str, direct: &str) -> Option<(String, String)> {
    let (base, token) = route(provider)?;
    let path = direct
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
        .unwrap_or("");
    Some((format!("{base}{path}"), token))
}

/// The model id to ask the proxy for while it serves `provider`: `id` when
/// the proxy lists it, else the newest dated snapshot of it the proxy lists
/// (`<id>-YYYYMMDD`, what an undated id means to Anthropic), else `id`. The
/// proxy knows older models only by their dated ids.
pub fn model_for(provider: &str, id: &str) -> String {
    match route(provider) {
        Some((base, token)) => newest(&listed(&base, &token), id),
        None => id.to_string(),
    }
}

fn newest(listed: &[String], id: &str) -> String {
    if listed.iter().any(|m| m == id) {
        return id.to_string();
    }
    listed
        .iter()
        .filter(|m| {
            m.strip_prefix(id)
                .and_then(|d| d.strip_prefix('-'))
                .is_some_and(|d| d.len() == 8 && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .max()
        .cloned()
        .unwrap_or_else(|| id.to_string())
}

/// The proxy's model ids, listed at most once a minute. An empty list (the
/// proxy still loading a key it was just given) isn't kept.
fn listed(base: &str, token: &str) -> Vec<String> {
    let mut cache = LISTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((listed_at, when, ids)) = cache.as_ref() {
        if listed_at == base && when.elapsed() < Duration::from_secs(60) {
            return ids.clone();
        }
    }
    let ids: Vec<String> = ureq::get(&format!("{base}/v1/models"))
        .timeout(Duration::from_secs(10))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .ok()
        .and_then(|r| r.into_json::<serde_json::Value>().ok())
        .and_then(|v| {
            v["data"].as_array().map(|models| {
                models
                    .iter()
                    .filter_map(|m| m["id"].as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default();
    if !ids.is_empty() {
        *cache = Some((base.to_string(), Instant::now(), ids.clone()));
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (A provider no plug uses: the route is process-wide, and other tests
    /// run beside this one.)
    #[test]
    fn a_provider_the_proxy_serves_goes_to_it_at_the_same_path() {
        set(Some(Route {
            base: "http://127.0.0.1:47791".into(),
            token: "t".into(),
            providers: vec!["w2-test".into()],
        }));
        assert_eq!(
            via("w2-test", "https://api.example.com/v1/messages"),
            Some(("http://127.0.0.1:47791/v1/messages".into(), "t".into()))
        );
        assert_eq!(via("w2-other", "https://x/v1beta"), None, "no key there");
        {
            let _agent = as_agent(Some("agent".into()));
            assert_eq!(route("w2-test").map(|r| r.1).as_deref(), Some("agent"));
        }
        assert_eq!(
            route("w2-test").map(|r| r.1).as_deref(),
            Some("t"),
            "scope ended"
        );
        set(None);
        assert_eq!(via("w2-test", "https://api.example.com/v1/messages"), None);
    }

    #[test]
    fn an_undated_id_the_proxy_lacks_asks_for_its_newest_snapshot() {
        let listed: Vec<String> = [
            "claude-sonnet-4-5-20250929",
            "claude-sonnet-4-5-20250101",
            "claude-sonnet-4-6",
            "claude-sonnet-4-5-thinking",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            newest(&listed, "claude-sonnet-4-5"),
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(
            newest(&listed, "claude-sonnet-4-6"),
            "claude-sonnet-4-6",
            "listed as is"
        );
        assert_eq!(
            newest(&listed, "claude-opus-9"),
            "claude-opus-9",
            "unknown stays"
        );
        assert_eq!(newest(&[], "claude-sonnet-4-5"), "claude-sonnet-4-5");
    }
}
