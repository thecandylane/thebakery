//! Node-owned cache over a linked provider's live model catalog (07d-A3D1
//! Phase B). `bakery_agent::Model::catalog()` is a real network round trip
//! for Grok/Claude/Gemini — this cache is what stands between that call and
//! anything an operator does in the client, so a picker can be opened
//! repeatedly (or `/studio/state` polled continuously, once a future phase
//! wires this in there too) without ever re-hitting the network on every
//! render. TTL + background refresh + last-known-good fallback: a stale
//! entry is served immediately while a refresh runs in the background: a
//! refresh that fails (real network error, not a provider-reported "no
//! credentials") never clobbers the last good catalog this node saw.
//!
//! Deliberately scoped to `grok`/`claude`/`gemini` plus the public
//! `claude_code_slot` account-adapter seam. The three API providers have
//! native list-models endpoints; Claude Code can report only what its
//! installed subscription adapter actually exposes. ACP/openai-compat plugs
//! are per-node operator config keyed by an arbitrary plug id and stay out of
//! this network cache.

use bakery_agent::{ClaudeModel, GeminiModel, GrokModel, Model, ModelCatalog, ModelCatalogEntry};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell};

/// File under the node's base dir holding what each account last reported.
pub(crate) const MEMORY_FILE: &str = "account-catalogs.json";
/// Bounds on what is remembered: a model list is small, and a file that
/// grew without limit would be keyed by whatever account ids ever appeared.
const MEMORY_MAX_ACCOUNTS: usize = 64;
const MEMORY_MAX_MODELS: usize = 200;
/// How long a failed probe of an account is not repeated: a picker that is
/// reopened must not start another adapter process each time.
const PROBE_RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize, Default)]
struct MemoryFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    accounts: HashMap<String, Vec<ModelCatalogEntry>>,
}

/// How long a cached catalog is served before a `get()` triggers a
/// background refresh. Long enough that repeated picker opens in one
/// operator session never re-hit the network; short enough that a newly
/// added key shows up within a session without a node restart.
const CATALOG_TTL: Duration = Duration::from_secs(300);

/// A fetch strategy: given a provider id, produce its current catalog or an
/// error describing why the attempt itself failed (never a "no credentials"
/// case — providers report that as `Ok(ModelCatalog::Unavailable { .. })`,
/// which is a legitimate value, not a failed fetch). A bare `fn` pointer
/// (not a closure) so it can move into `spawn_blocking` untouched and so
/// tests can substitute a deterministic fetcher with no real network I/O.
pub(crate) type CatalogFetcher = fn(&str) -> Result<ModelCatalog, String>;

#[derive(Clone)]
struct CacheEntry {
    catalog: ModelCatalog,
    fetched_at: Instant,
    /// Set while a background refresh for this provider is already in
    /// flight, so a second stale `get()` before it lands doesn't spawn a
    /// second concurrent fetch against the same endpoint.
    refreshing: bool,
}

#[derive(Default)]
pub(crate) struct ModelCatalogCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
    /// One-time coordination gate per provider id's very first fetch (07d-
    /// A3D1 review: without this, two concurrent `get()` calls for the same
    /// not-yet-cached id each independently perform their own live network
    /// fetch, and the loser's result is silently discarded rather than
    /// cached). The first caller for an id creates and drives this cell;
    /// every concurrent caller for the same id awaits the SAME cell instead
    /// of starting a second redundant fetch. Removed once the fetch lands so
    /// this map never grows past however many distinct ids are *currently*
    /// mid-first-fetch.
    in_flight: Mutex<HashMap<String, Arc<OnceCell<ModelCatalog>>>>,
    /// What each account last reported, kept across restarts. A node restarts
    /// on every deploy, and a native account only knows its models while a
    /// conversation has a live session on it — so without this the model
    /// picker was empty after every restart, and offline. Never a guess: a
    /// list is stored only when an account actually reported it.
    remembered: std::sync::Mutex<HashMap<String, Vec<ModelCatalogEntry>>>,
    /// Where `remembered` is saved; none keeps it in memory only (tests).
    remembered_path: Option<PathBuf>,
    /// Accounts whose probe failed, when and why.
    probe_failures: std::sync::Mutex<HashMap<String, (Instant, String)>>,
    /// One probe at a time, node-wide: each starts an adapter process.
    probe_gate: Mutex<()>,
}

/// Provider ids this cache is actually willing to remember (`grok`/`claude`/
/// `gemini`, plus the bounded Claude Code account slot). `entries`
/// has no eviction, so caching a result for anything else — `acp`, a plug
/// id, a typo'd string, or literally any other caller-chosen
/// text reaching `routes::studio::model_catalog`'s query param — would grow
/// it forever, keyed by arbitrary external input (07d-A3D1 review). Callers
/// outside this module (the route handler) must check this before ever
/// calling [`ModelCatalogCache::get`], and answer an ineligible id directly
/// via [`fetch_live_provider_catalog`] instead — cheap, since `model_for`
/// returns `None` immediately with no I/O for anything not in this list.
pub(crate) fn is_cache_eligible(provider_id: &str) -> bool {
    matches!(provider_id, "grok" | "claude" | "claude-code" | "gemini")
}

pub(crate) fn unavailable(provider_id: &str, reason: String) -> ModelCatalog {
    ModelCatalog::Unavailable {
        default: ModelCatalogEntry {
            id: provider_id.to_string(),
            label: provider_id.to_string(),
            context_window: None,
            effort_levels: Vec::new(),
        },
        reason,
    }
}

fn model_for(provider_id: &str) -> Option<Arc<dyn Model>> {
    match provider_id {
        "grok" => Some(Arc::new(GrokModel::from_env())),
        "claude" => Some(Arc::new(ClaudeModel::from_env())),
        "gemini" => Some(Arc::new(GeminiModel::from_env())),
        _ => None,
    }
}

/// The real fetcher `routes::studio` wires in. Claude Code delegates through
/// its public account slot; ordinary HTTP providers use their `Model`
/// implementation. Every other id has no live catalog builder here.
pub(crate) fn fetch_live_provider_catalog(provider_id: &str) -> Result<ModelCatalog, String> {
    if provider_id == "claude-code" {
        return bakery_agent::claude_code_slot::catalog();
    }
    match model_for(provider_id) {
        Some(model) => model.catalog(),
        None => Err("this model does not report a live catalog".to_string()),
    }
}

impl ModelCatalogCache {
    /// A cache whose remembered catalogs live in `path` (read now, written
    /// whenever one changes). An unreadable file is simply not used.
    pub(crate) fn persisted(path: PathBuf) -> Self {
        let remembered = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<MemoryFile>(&bytes).ok())
            .map(|file| file.accounts)
            .unwrap_or_default();
        Self {
            remembered: std::sync::Mutex::new(remembered),
            remembered_path: Some(path),
            ..Self::default()
        }
    }

    /// The models `account` last reported, if it ever has.
    pub(crate) fn recall(&self, account: &str) -> Option<Vec<ModelCatalogEntry>> {
        self.remembered.lock().unwrap().get(account).cloned()
    }

    /// Remembers what `account` just reported. An empty list is not a
    /// report, so it never replaces a real one.
    pub(crate) fn remember(&self, account: &str, models: &[ModelCatalogEntry]) {
        if models.is_empty() {
            return;
        }
        let models: Vec<ModelCatalogEntry> = models.iter().take(MEMORY_MAX_MODELS).cloned().collect();
        let snapshot = {
            let mut remembered = self.remembered.lock().unwrap();
            if remembered.get(account).is_some_and(|known| {
                serde_json::to_string(known).ok() == serde_json::to_string(&models).ok()
            }) {
                return;
            }
            if !remembered.contains_key(account) && remembered.len() >= MEMORY_MAX_ACCOUNTS {
                return;
            }
            remembered.insert(account.to_string(), models);
            remembered.clone()
        };
        let Some(path) = &self.remembered_path else {
            return;
        };
        let file = MemoryFile {
            version: 1,
            accounts: snapshot,
        };
        let Ok(bytes) = serde_json::to_vec_pretty(&file) else {
            return;
        };
        // Whole file or nothing: a half-written list must never be read back.
        let tmp = path.with_extension("json.tmp");
        let written = std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
        if let Err(error) = written {
            eprintln!("bakery-node: could not save {}: {error}", path.display());
        }
    }

    /// Why `account`'s last probe failed, while that is recent enough not to
    /// try again.
    pub(crate) fn recent_probe_failure(&self, account: &str) -> Option<String> {
        let failures = self.probe_failures.lock().unwrap();
        let (at, reason) = failures.get(account)?;
        (at.elapsed() < PROBE_RETRY_AFTER).then(|| reason.clone())
    }

    pub(crate) fn note_probe_failure(&self, account: &str, reason: &str) {
        self.probe_failures
            .lock()
            .unwrap()
            .insert(account.to_string(), (Instant::now(), reason.to_string()));
    }

    pub(crate) fn clear_probe_failure(&self, account: &str) {
        self.probe_failures.lock().unwrap().remove(account);
    }

    /// Held for the length of one probe.
    pub(crate) fn probe_gate(&self) -> &Mutex<()> {
        &self.probe_gate
    }

    /// Best currently-known catalog for `provider_id`. The very first call
    /// for a provider fetches inline (nothing cached yet to serve) — and if
    /// a second call for the same never-yet-cached id arrives concurrently,
    /// it awaits that SAME in-flight fetch rather than starting its own.
    /// Every call after that is answered from cache immediately — fresh data
    /// as-is, or the last-known-good catalog while a background refresh (at
    /// most one in flight per provider at a time) brings it up to date.
    pub(crate) async fn get(
        self: Arc<Self>,
        provider_id: &str,
        fetcher: CatalogFetcher,
    ) -> ModelCatalog {
        {
            let mut guard = self.entries.lock().await;
            if let Some(entry) = guard.get_mut(provider_id) {
                let current = entry.catalog.clone();
                if entry.fetched_at.elapsed() >= CATALOG_TTL && !entry.refreshing {
                    entry.refreshing = true;
                    spawn_background_refresh(Arc::clone(&self), provider_id.to_string(), fetcher);
                }
                return current;
            }
        }

        let cell = {
            let mut pending = self.in_flight.lock().await;
            Arc::clone(
                pending
                    .entry(provider_id.to_string())
                    .or_insert_with(|| Arc::new(OnceCell::new())),
            )
        };
        let owned_id = provider_id.to_string();
        let this = Arc::clone(&self);
        let catalog = cell
            .get_or_init(|| async move {
                let id = owned_id.clone();
                let result = tokio::task::spawn_blocking(move || fetcher(&id))
                    .await
                    .unwrap_or_else(|e| Err(format!("catalog fetch task panicked: {e}")));
                match result {
                    Ok(catalog) => {
                        if let ModelCatalog::Available { models } = &catalog {
                            this.remember(&owned_id, models);
                        }
                        catalog
                    }
                    // The fetch itself failed (offline, a provider outage —
                    // not "no credentials", which is a real answer): the
                    // last list this account reported is still the best one.
                    Err(reason) => match this.recall(&owned_id) {
                        Some(models) => ModelCatalog::Available { models },
                        None => unavailable(&owned_id, reason),
                    },
                }
            })
            .await
            .clone();

        let mut guard = self.entries.lock().await;
        guard
            .entry(provider_id.to_string())
            .or_insert_with(|| CacheEntry {
                catalog: catalog.clone(),
                fetched_at: Instant::now(),
                refreshing: false,
            });
        drop(guard);
        self.in_flight.lock().await.remove(provider_id);
        catalog
    }

    /// A specific model's context window from whatever is already cached
    /// for `provider_id` — never triggers a fetch (07d-A3D1 Phase C), so a
    /// turn's completion never stalls on network for this. Read via
    /// `blocking_lock` since every caller runs inside the same
    /// `spawn_blocking` closure a turn already executes in
    /// (`routes::agent`). `None` when the provider isn't cached at all, or
    /// the cached catalog doesn't mention this exact model id.
    pub(crate) fn cached_context_window(&self, provider_id: &str, model_id: &str) -> Option<u64> {
        let guard = self.entries.blocking_lock();
        match &guard.get(provider_id)?.catalog {
            ModelCatalog::Available { models } => models
                .iter()
                .find(|m| m.id == model_id)
                .and_then(|m| m.context_window),
            ModelCatalog::Unavailable { default, .. } => (default.id == model_id)
                .then_some(default.context_window)
                .flatten(),
        }
    }
}

fn spawn_background_refresh(
    cache: Arc<ModelCatalogCache>,
    provider_id: String,
    fetcher: CatalogFetcher,
) {
    tokio::spawn(async move {
        let id = provider_id.clone();
        let result = tokio::task::spawn_blocking(move || fetcher(&id))
            .await
            .unwrap_or_else(|e| Err(format!("catalog refresh task panicked: {e}")));

        let mut guard = cache.entries.lock().await;
        if let Some(entry) = guard.get_mut(&provider_id) {
            // A failed refresh (real network error) never overwrites a
            // prior good catalog — last-known-good stays in place. Only the
            // retry timer resets, so a failing endpoint isn't re-hit on
            // every subsequent stale read.
            if let Ok(fresh) = result {
                if let ModelCatalog::Available { models } = &fresh {
                    cache.remember(&provider_id, models);
                }
                entry.catalog = fresh;
            }
            entry.fetched_at = Instant::now();
            entry.refreshing = false;
        }
    });
}

#[cfg(test)]
impl ModelCatalogCache {
    /// Seeds a cache entry directly, pre-aged by `age` — lets a test put the
    /// cache straight into a "stale" or "fresh" state without a real sleep.
    /// `pub(crate)` (not just private to this module's own tests) so
    /// `routes::agent`'s integration tests can seed a catalog entry too
    /// (07d-A3D1 Phase C, `context_window` composition).
    pub(crate) async fn seed_for_test(
        &self,
        provider_id: &str,
        catalog: ModelCatalog,
        age: Duration,
    ) {
        let mut guard = self.entries.lock().await;
        guard.insert(
            provider_id.to_string(),
            CacheEntry {
                catalog,
                fetched_at: Instant::now() - age,
                refreshing: false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn available_with(id: &str) -> ModelCatalog {
        ModelCatalog::Available {
            models: vec![ModelCatalogEntry {
                id: id.to_string(),
                label: id.to_string(),
                context_window: None,
                effort_levels: Vec::new(),
            }],
        }
    }

    fn only_available(catalog: &ModelCatalog) -> &str {
        match catalog {
            ModelCatalog::Available { models } => models[0].id.as_str(),
            ModelCatalog::Unavailable { .. } => panic!("expected Available"),
        }
    }

    static FIRST_CALL_COUNT: AtomicU32 = AtomicU32::new(0);
    fn fetch_counting_first_call(_id: &str) -> Result<ModelCatalog, String> {
        FIRST_CALL_COUNT.fetch_add(1, Ordering::SeqCst);
        Ok(available_with("counted-model"))
    }

    #[tokio::test]
    async fn a_fresh_entry_is_served_without_calling_the_fetcher_again() {
        let cache = Arc::new(ModelCatalogCache::default());
        let first = cache
            .clone()
            .get("test:first", fetch_counting_first_call)
            .await;
        assert_eq!(only_available(&first), "counted-model");
        assert_eq!(FIRST_CALL_COUNT.load(Ordering::SeqCst), 1);

        let second = cache
            .clone()
            .get("test:first", fetch_counting_first_call)
            .await;
        assert_eq!(only_available(&second), "counted-model");
        assert_eq!(
            FIRST_CALL_COUNT.load(Ordering::SeqCst),
            1,
            "a fresh entry must not re-invoke the fetcher"
        );
    }

    fn fetch_never_called(_id: &str) -> Result<ModelCatalog, String> {
        panic!("fetcher must not be called for an already-fresh entry");
    }

    #[tokio::test]
    async fn a_missing_provider_id_degrades_to_unavailable_instead_of_erroring() {
        fn fetch_no_catalog(_id: &str) -> Result<ModelCatalog, String> {
            Err("this model does not report a live catalog".to_string())
        }
        let cache = Arc::new(ModelCatalogCache::default());
        let catalog = cache.get("claude-code", fetch_no_catalog).await;
        match catalog {
            ModelCatalog::Unavailable { default, reason } => {
                assert_eq!(default.id, "claude-code");
                assert_eq!(reason, "this model does not report a live catalog");
            }
            ModelCatalog::Available { .. } => panic!("expected Unavailable"),
        }
    }

    fn fetch_fresh_data(_id: &str) -> Result<ModelCatalog, String> {
        Ok(available_with("fresh-model"))
    }

    #[tokio::test]
    async fn a_stale_entry_is_served_immediately_then_a_background_refresh_updates_it() {
        let cache = Arc::new(ModelCatalogCache::default());
        cache
            .seed_for_test(
                "test:stale",
                available_with("old-model"),
                CATALOG_TTL + Duration::from_secs(1),
            )
            .await;

        let served = cache.clone().get("test:stale", fetch_fresh_data).await;
        assert_eq!(
            only_available(&served),
            "old-model",
            "a stale read must return last-known-good immediately"
        );

        let updated = wait_until(&cache, "test:stale", "fresh-model").await;
        assert!(
            updated,
            "background refresh must eventually replace the cached catalog"
        );

        // The now-fresh entry must not trigger yet another fetch.
        let again = cache.clone().get("test:stale", fetch_never_called).await;
        assert_eq!(only_available(&again), "fresh-model");
    }

    #[tokio::test]
    async fn a_failed_background_refresh_preserves_the_last_known_good_catalog() {
        fn fetch_network_down(_id: &str) -> Result<ModelCatalog, String> {
            Err("network down".to_string())
        }
        let cache = Arc::new(ModelCatalogCache::default());
        cache
            .seed_for_test(
                "test:flaky",
                available_with("still-good"),
                CATALOG_TTL + Duration::from_secs(1),
            )
            .await;

        let served = cache.clone().get("test:flaky", fetch_network_down).await;
        assert_eq!(only_available(&served), "still-good");

        // Give the spawned background refresh a bounded window to run and
        // fail; the cached catalog must be exactly what it was before.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let after = cache.clone().get("test:flaky", fetch_never_called).await;
        assert_eq!(
            only_available(&after),
            "still-good",
            "a failed refresh must never clobber last-known-good"
        );
    }

    #[tokio::test]
    async fn a_second_stale_read_before_the_first_refresh_lands_does_not_spawn_another_fetch() {
        static REFRESH_CALLS: AtomicU32 = AtomicU32::new(0);
        fn fetch_counting_refresh(_id: &str) -> Result<ModelCatalog, String> {
            REFRESH_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(available_with("refreshed-once"))
        }
        let cache = Arc::new(ModelCatalogCache::default());
        cache
            .seed_for_test(
                "test:dedup",
                available_with("old"),
                CATALOG_TTL + Duration::from_secs(1),
            )
            .await;

        let _ = cache
            .clone()
            .get("test:dedup", fetch_counting_refresh)
            .await;
        let _ = cache
            .clone()
            .get("test:dedup", fetch_counting_refresh)
            .await;

        assert!(wait_until(&cache, "test:dedup", "refreshed-once").await);
        assert_eq!(
            REFRESH_CALLS.load(Ordering::SeqCst),
            1,
            "only one refresh should have run"
        );
    }

    static COLD_START_CALLS: AtomicU32 = AtomicU32::new(0);
    fn fetch_slow_cold_start(_id: &str) -> Result<ModelCatalog, String> {
        COLD_START_CALLS.fetch_add(1, Ordering::SeqCst);
        // Widens the race window: without this, the winner could finish
        // before the second `get()` call ever reaches `in_flight`, and the
        // test would pass by accident rather than by exercising the dedup
        // path. Runs on a real spawn_blocking thread, so a real sleep here
        // never blocks the tokio runtime itself.
        std::thread::sleep(Duration::from_millis(50));
        Ok(available_with("cold-started"))
    }

    /// 07d-A3D1 review: two `get()` calls racing for the same id that has
    /// never been cached before must only fetch once — the loser awaits the
    /// winner's in-flight fetch instead of starting (and then discarding)
    /// its own.
    #[tokio::test]
    async fn two_concurrent_cold_calls_for_the_same_provider_only_fetch_once() {
        let cache = Arc::new(ModelCatalogCache::default());
        let (a, b) = (cache.clone(), cache.clone());
        let (r1, r2) = tokio::join!(
            a.get("test:cold-race", fetch_slow_cold_start),
            b.get("test:cold-race", fetch_slow_cold_start),
        );
        assert_eq!(only_available(&r1), "cold-started");
        assert_eq!(only_available(&r2), "cold-started");
        assert_eq!(
            COLD_START_CALLS.load(Ordering::SeqCst),
            1,
            "only one of the two racing cold calls should have actually fetched"
        );
    }

    /// Polls (bounded, real but short sleeps) until `provider_id`'s cached
    /// catalog's single model id equals `want_id`, or gives up.
    async fn wait_until(cache: &Arc<ModelCatalogCache>, provider_id: &str, want_id: &str) -> bool {
        for _ in 0..50 {
            let entries = cache.entries.lock().await;
            if let Some(entry) = entries.get(provider_id) {
                if only_available(&entry.catalog) == want_id {
                    return true;
                }
            }
            drop(entries);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// `cached_context_window` uses `blocking_lock` (by design — it must be
    /// callable from inside the `spawn_blocking` closure a real turn runs
    /// in, never from an actively-polled async task), so it cannot itself
    /// be called from directly within a `#[tokio::test]`. These two tests
    /// use a throwaway runtime just to run the async `seed_for_test` setup
    /// step, then call `cached_context_window` afterward on the plain OS
    /// thread — exactly mirroring how `routes::agent`'s real sync turn code
    /// calls it.
    #[test]
    fn cached_context_window_reads_a_seeded_entry_without_fetching() {
        let cache = ModelCatalogCache::default();
        let catalog = ModelCatalog::Available {
            models: vec![
                ModelCatalogEntry {
                    id: "model-a".into(),
                    label: "A".into(),
                    context_window: Some(128_000),
                    effort_levels: Vec::new(),
                },
                ModelCatalogEntry {
                    id: "model-b".into(),
                    label: "B".into(),
                    context_window: None,
                    effort_levels: Vec::new(),
                },
            ],
        };
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(cache.seed_for_test("claude", catalog, Duration::from_secs(0)));

        assert_eq!(
            cache.cached_context_window("claude", "model-a"),
            Some(128_000)
        );
        assert_eq!(
            cache.cached_context_window("claude", "model-b"),
            None,
            "present but with no context_window itself"
        );
        assert_eq!(cache.cached_context_window("claude", "no-such-model"), None);
        assert_eq!(
            cache.cached_context_window("gemini", "model-a"),
            None,
            "a different, never-cached provider id"
        );
    }

    #[test]
    fn cached_context_window_reads_the_unavailable_default_when_it_matches() {
        let cache = ModelCatalogCache::default();
        let catalog = ModelCatalog::Unavailable {
            default: ModelCatalogEntry {
                id: "grok-4.5".into(),
                label: "grok-4.5".into(),
                context_window: Some(2_000_000),
                effort_levels: Vec::new(),
            },
            reason: "no credentials".into(),
        };
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(cache.seed_for_test("grok", catalog, Duration::from_secs(0)));

        assert_eq!(
            cache.cached_context_window("grok", "grok-4.5"),
            Some(2_000_000)
        );
        assert_eq!(
            cache.cached_context_window("grok", "some-other-model"),
            None,
            "Unavailable's default only matches its own id"
        );
    }
}
