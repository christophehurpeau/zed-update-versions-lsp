use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Mutex, Notify, RwLock};

/// Result of a version lookup from a registry.
#[derive(Debug, Clone)]
pub struct VersionResult {
    /// All stable (non-prerelease, non-yanked) versions, sorted descending (newest first).
    pub stable_versions: Vec<String>,
    pub prerelease: Option<String>,
}

struct CacheEntry {
    result: VersionResult,
    inserted_at: Instant,
}

/// In-memory cache with TTL and inflight deduplication.
pub struct VersionCache {
    store: RwLock<HashMap<String, CacheEntry>>,
    inflight: Mutex<HashMap<String, broadcast::Sender<VersionResult>>>,
    /// TTL stored in milliseconds so it can be updated atomically at runtime.
    ttl_ms: AtomicU64,
    /// Notified whenever an entry is inserted; used by the background sweep
    /// task to avoid running any timers while the cache is empty.
    populated: Notify,
}

impl VersionCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            inflight: Mutex::new(HashMap::new()),
            ttl_ms: AtomicU64::new(ttl.as_millis() as u64),
            populated: Notify::new(),
        }
    }

    /// Update the TTL for future cache lookups (hot-reload support).
    pub fn update_ttl(&self, secs: u64) {
        self.ttl_ms
            .store(secs.saturating_mul(1_000), Ordering::Relaxed);
    }

    /// Get a cached entry if it exists and hasn't expired.
    pub async fn get(&self, key: &str) -> Option<VersionResult> {
        let ttl = Duration::from_millis(self.ttl_ms.load(Ordering::Relaxed));
        let store = self.store.read().await;
        if let Some(entry) = store.get(key) {
            if entry.inserted_at.elapsed() < ttl {
                return Some(entry.result.clone());
            }
        }
        None
    }

    /// Insert or update a cache entry.
    pub async fn set(&self, key: String, result: VersionResult) {
        self.store.write().await.insert(
            key,
            CacheEntry {
                result,
                inserted_at: Instant::now(),
            },
        );
        // Wake the background sweep task if it is dormant.
        self.populated.notify_one();
    }

    pub async fn is_empty(&self) -> bool {
        self.store.read().await.is_empty()
    }

    /// Blocks until at least one entry has been inserted since the last time
    /// this returned.  Returns immediately if a permit is already queued
    /// (i.e. `set` was called while no one was waiting).
    pub async fn wait_until_populated(&self) {
        self.populated.notified().await;
    }

    /// Remove all entries that have exceeded the TTL.
    /// Skips the write lock entirely when the cache is empty.
    pub async fn purge_expired(&self) {
        // Cheap read-lock check — avoids acquiring an exclusive write lock when
        // there is nothing to clean up.
        if self.store.read().await.is_empty() {
            return;
        }
        let ttl = Duration::from_millis(self.ttl_ms.load(Ordering::Relaxed));
        self.store
            .write()
            .await
            .retain(|_, entry| entry.inserted_at.elapsed() < ttl);
    }

    /// Resolve a version, using cache and inflight deduplication.
    /// If the value is cached, return it. If an inflight request exists, wait for it.
    /// Otherwise, call the fetcher and cache the result.
    pub async fn resolve<F, Fut>(&self, key: &str, fetcher: F) -> VersionResult
    where
        F: FnOnce() -> Fut + Send,
        Fut: std::future::Future<Output = VersionResult> + Send,
    {
        // Check cache first
        if let Some(cached) = self.get(key).await {
            return cached;
        }

        // Check for inflight request
        {
            let inflight = self.inflight.lock().await;
            if let Some(tx) = inflight.get(key) {
                let mut rx = tx.subscribe();
                drop(inflight);
                if let Ok(result) = rx.recv().await {
                    return result;
                }
                // If recv failed, the sender was dropped — fall through to fetch
            }
        }

        // Register ourselves as the inflight fetcher
        let (tx, _) = broadcast::channel(1);
        {
            let mut inflight = self.inflight.lock().await;
            inflight.insert(key.to_string(), tx.clone());
        }

        // Fetch
        let result = fetcher().await;

        // Cache the result
        self.set(key.to_string(), result.clone()).await;

        // Notify waiters and remove inflight entry
        let _ = tx.send(result.clone());
        {
            let mut inflight = self.inflight.lock().await;
            inflight.remove(key);
        }

        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_cache_get_miss() {
        let cache = VersionCache::new(Duration::from_secs(300));
        assert!(cache.get("npm:react").await.is_none());
    }

    #[tokio::test]
    async fn test_cache_set_and_get() {
        let cache = VersionCache::new(Duration::from_secs(300));
        let result = VersionResult {
            stable_versions: vec!["18.2.0".to_string()],
            prerelease: None,
        };
        cache.set("npm:react".to_string(), result).await;

        let cached = cache.get("npm:react").await.unwrap();
        assert_eq!(
            cached.stable_versions.first().map(String::as_str),
            Some("18.2.0")
        );
        assert!(cached.prerelease.is_none());
    }

    #[tokio::test]
    async fn test_cache_ttl_expiry() {
        let cache = VersionCache::new(Duration::from_millis(50));
        cache
            .set(
                "npm:react".to_string(),
                VersionResult {
                    stable_versions: vec!["18.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        assert!(cache.get("npm:react").await.is_some());
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(cache.get("npm:react").await.is_none());
    }

    /// resolve() must return a cached entry without invoking the fetcher.
    #[tokio::test]
    async fn test_cache_resolve_returns_cached_without_fetching() {
        let cache = VersionCache::new(Duration::from_secs(300));
        cache
            .set(
                "npm:react".to_string(),
                VersionResult {
                    stable_versions: vec!["18.2.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        let call_count = Arc::new(AtomicU32::new(0));
        let count = call_count.clone();
        let result = cache
            .resolve("npm:react", || async move {
                count.fetch_add(1, Ordering::Relaxed);
                VersionResult {
                    stable_versions: vec!["99.0.0".to_string()],
                    prerelease: None,
                }
            })
            .await;

        assert_eq!(
            call_count.load(Ordering::Relaxed),
            0,
            "fetcher must not be called on a cache hit"
        );
        assert_eq!(
            result.stable_versions.first().map(String::as_str),
            Some("18.2.0")
        );
    }

    /// After a resolve() completes, the result must be stored so that a second
    /// sequential resolve() for the same key never calls the fetcher again.
    #[tokio::test]
    async fn test_cache_resolve_caches_result() {
        let cache = VersionCache::new(Duration::from_secs(300));
        let call_count = Arc::new(AtomicU32::new(0));

        // First call — fetcher runs
        {
            let count = call_count.clone();
            cache
                .resolve("npm:lodash", || async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    VersionResult {
                        stable_versions: vec!["4.17.21".to_string()],
                        prerelease: None,
                    }
                })
                .await;
        }
        assert_eq!(call_count.load(Ordering::Relaxed), 1);

        // Second call — must return cached entry without calling the fetcher
        {
            let count = call_count.clone();
            let result = cache
                .resolve("npm:lodash", || async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    VersionResult {
                        stable_versions: vec!["9.9.9".to_string()],
                        prerelease: None,
                    }
                })
                .await;
            assert_eq!(
                result.stable_versions.first().map(String::as_str),
                Some("4.17.21"),
                "second call must return the originally cached version"
            );
        }
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "fetcher must not be called a second time"
        );
    }

    /// When all senders for an in-flight key are dropped before delivering a
    /// result (e.g. the fetching task panicked), a waiting resolve() must detect
    /// the closed channel and fall through to issue its own fetch instead of
    /// hanging forever.
    #[tokio::test]
    async fn test_cache_resolve_fallthrough_on_sender_drop() {
        let cache = Arc::new(VersionCache::new(Duration::from_secs(300)));
        let call_count = Arc::new(AtomicU32::new(0));

        // Manually plant a live sender in the inflight map, simulating another
        // task that registered but has not yet delivered a value.
        let (tx, _) = broadcast::channel::<VersionResult>(1);
        cache
            .inflight
            .lock()
            .await
            .insert("npm:lodash".to_string(), tx.clone());

        // Spawn a resolve task — it will find the inflight entry, subscribe to
        // the channel, and block on rx.recv().
        let cache2 = cache.clone();
        let count = call_count.clone();
        let handle = tokio::spawn(async move {
            cache2
                .resolve("npm:lodash", || async move {
                    count.fetch_add(1, Ordering::Relaxed);
                    VersionResult {
                        stable_versions: vec!["4.17.21".to_string()],
                        prerelease: None,
                    }
                })
                .await
        });

        // Yield long enough for the spawned task to subscribe and park on recv().
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Drop ALL sender instances: the clone held by the inflight map and the
        // one held locally.  Once both are gone the broadcast channel is closed
        // and rx.recv() in the spawned task returns Err(RecvError::Closed).
        cache.inflight.lock().await.remove("npm:lodash");
        drop(tx);

        let result = handle.await.unwrap();
        assert_eq!(
            call_count.load(Ordering::Relaxed),
            1,
            "fetcher must be called after the in-flight sender is dropped"
        );
        assert_eq!(
            result.stable_versions.first().map(String::as_str),
            Some("4.17.21")
        );
        // The result must be cached afterwards so a repeat call is free.
        assert!(cache.get("npm:lodash").await.is_some());
    }

    #[tokio::test]
    async fn test_cache_resolve_deduplication() {
        let cache = Arc::new(VersionCache::new(Duration::from_secs(300)));
        let call_count = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..5 {
            let cache = cache.clone();
            let count = call_count.clone();
            handles.push(tokio::spawn(async move {
                cache
                    .resolve("npm:react", || {
                        let count = count.clone();
                        async move {
                            count.fetch_add(1, Ordering::Relaxed);
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            VersionResult {
                                stable_versions: vec!["18.2.0".to_string()],
                                prerelease: None,
                            }
                        }
                    })
                    .await
            }));
        }

        for handle in handles {
            let r = handle.await.unwrap();
            assert_eq!(
                r.stable_versions.first().map(String::as_str),
                Some("18.2.0")
            );
        }

        // The fetcher should have been called only once (or at most a few if
        // timing is tight, but never 5 times)
        let count = call_count.load(Ordering::Relaxed);
        assert!(count <= 2, "Fetcher called {count} times, expected ≤ 2");
    }

    /// Reducing the TTL via update_ttl should cause previously-valid entries to
    /// be treated as expired on the next get().
    #[tokio::test]
    async fn test_update_ttl_shortens_expiry() {
        let cache = VersionCache::new(Duration::from_secs(300));
        cache
            .set(
                "npm:react".to_string(),
                VersionResult {
                    stable_versions: vec!["18.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        // Entry is valid with the original long TTL.
        assert!(cache.get("npm:react").await.is_some());

        // Shrink TTL to 0 — every existing entry is now expired.
        cache.update_ttl(0);
        assert!(
            cache.get("npm:react").await.is_none(),
            "entry should be expired after TTL reduced to 0"
        );
    }

    /// Extending the TTL via update_ttl should keep entries alive that would
    /// otherwise have expired under the original TTL.
    #[tokio::test]
    async fn test_update_ttl_extends_expiry() {
        // Start with a very short TTL so the entry expires quickly.
        let cache = VersionCache::new(Duration::from_millis(30));
        cache
            .set(
                "npm:react".to_string(),
                VersionResult {
                    stable_versions: vec!["18.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        // Before expiry the entry is present.
        assert!(cache.get("npm:react").await.is_some());

        // Extend TTL to a large value before the original 30 ms elapses.
        cache.update_ttl(300);

        // Wait past the original 30 ms window.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Entry must still be alive because the new TTL is 300 s.
        assert!(
            cache.get("npm:react").await.is_some(),
            "entry should still be valid after TTL was extended"
        );
    }

    /// purge_expired must physically remove entries that have outlived the TTL
    /// and leave live entries untouched.
    #[tokio::test]
    async fn test_purge_expired_removes_stale_entries() {
        let cache = VersionCache::new(Duration::from_millis(30));
        cache
            .set(
                "npm:stale".to_string(),
                VersionResult {
                    stable_versions: vec!["1.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Insert a fresh entry after the original one has expired.
        cache
            .set(
                "npm:fresh".to_string(),
                VersionResult {
                    stable_versions: vec!["2.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;

        cache.purge_expired().await;

        assert!(
            cache.store.read().await.get("npm:stale").is_none(),
            "stale entry must be physically removed"
        );
        assert!(
            cache.store.read().await.get("npm:fresh").is_some(),
            "live entry must survive purge"
        );
    }

    /// purge_expired on an empty cache must be a no-op (no panic, no write lock).
    #[tokio::test]
    async fn test_purge_expired_empty_cache() {
        let cache = VersionCache::new(Duration::from_secs(300));
        cache.purge_expired().await; // must not panic
        assert!(cache.store.read().await.is_empty());
    }

    /// wait_until_populated must return immediately when a permit was already
    /// queued by a prior set() call, and must block until set() is called when
    /// the cache starts empty.
    #[tokio::test]
    async fn test_wait_until_populated() {
        let cache = Arc::new(VersionCache::new(Duration::from_secs(300)));

        // Case 1: permit already queued — returns without spawning anything.
        cache
            .set(
                "npm:react".to_string(),
                VersionResult {
                    stable_versions: vec!["18.0.0".to_string()],
                    prerelease: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_millis(10), cache.wait_until_populated())
            .await
            .expect("must return immediately when permit is already queued");

        // Case 2: cache starts empty — blocks until set() fires.
        let cache2 = Arc::new(VersionCache::new(Duration::from_secs(300)));
        let cache3 = Arc::clone(&cache2);
        let handle = tokio::spawn(async move {
            cache3.wait_until_populated().await;
        });
        // Give the spawned task time to park on notified().
        tokio::time::sleep(Duration::from_millis(10)).await;
        cache2
            .set(
                "npm:lodash".to_string(),
                VersionResult {
                    stable_versions: vec!["4.17.21".to_string()],
                    prerelease: None,
                },
            )
            .await;
        tokio::time::timeout(Duration::from_millis(100), handle)
            .await
            .expect("task must complete after set() is called")
            .unwrap();
    }
}
