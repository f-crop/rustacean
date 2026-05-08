//! Installation token cache (REQ-GH-05, ADR-005 §6).
//!
//! In-process per-installation cache for GitHub App installation access
//! tokens. Tokens live ~60 min upstream; we treat any token with less than
//! 10 min of life left as expired and re-mint, giving callers an effective
//! 50 min usable TTL with a 10 min safety margin.
//!
//! Lookups are lock-free `DashMap` reads on the warm path; cold misses are
//! collapsed via per-installation single-flight to avoid stampedes on
//! GitHub's `/app/installations/{id}/access_tokens` endpoint (~200 ms RTT).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;

use crate::error::GhError;
use crate::secret::Secret;

/// Boxed future returned by [`TokenMinter::mint`]. Pinned and `Send` so the
/// cache can hold `Arc<dyn TokenMinter>` and drive the future across tasks.
pub type MintFuture<'a> = Pin<Box<dyn Future<Output = Result<CachedToken, GhError>> + Send + 'a>>;

/// Treat tokens with less than this remaining lifetime as expired.
pub const SAFETY_MARGIN: Duration = Duration::from_secs(10 * 60);

/// Periodic sweep cadence for evicting expired tokens (ADR-005 §6.5).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// A GitHub installation access token with its absolute expiry (UTC).
#[derive(Debug, Clone)]
pub struct CachedToken {
    pub token: Secret<String>,
    pub expires_at: DateTime<Utc>,
}

/// Mints a fresh installation token. Implementors typically perform an
/// App-JWT exchange against `POST /app/installations/{id}/access_tokens`.
///
/// Returns a [`MintFuture`] rather than `async fn` so the trait stays
/// object-safe for `Arc<dyn TokenMinter>`.
pub trait TokenMinter: Send + Sync {
    fn mint(&self, installation_id: i64) -> MintFuture<'_>;
}

/// Per-installation in-process token cache with single-flight mint.
pub struct TokenCache {
    inner: DashMap<i64, CachedToken>,
    /// Per-installation mutex used to collapse concurrent cold-misses.
    /// Contended only on cache miss; warm path never touches this map.
    single_flight: DashMap<i64, Arc<AsyncMutex<()>>>,
    minter: Arc<dyn TokenMinter>,
    /// Background sweep handle. Aborted on drop.
    sweep: StdMutex<Option<JoinHandle<()>>>,
}

impl TokenCache {
    #[must_use]
    pub fn new(minter: Arc<dyn TokenMinter>) -> Arc<Self> {
        Arc::new(Self {
            inner: DashMap::new(),
            single_flight: DashMap::new(),
            minter,
            sweep: StdMutex::new(None),
        })
    }

    /// Returns a usable installation token, minting if absent or near expiry.
    ///
    /// # Errors
    ///
    /// Propagates any [`GhError`] returned by the minter (JWT failure,
    /// HTTP error, GitHub 4xx/5xx).
    pub async fn get_or_mint(&self, installation_id: i64) -> Result<Secret<String>, GhError> {
        if let Some(t) = self.read_fresh(installation_id) {
            return Ok(t);
        }

        let lock = self
            .single_flight
            .entry(installation_id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone();
        // review-checklist-ok: async-guard — singleflight pattern; tokio::sync::Mutex is designed for cross-await holding
        let _guard = lock.lock().await;

        // Re-check: a concurrent caller may have minted while we were queued.
        if let Some(t) = self.read_fresh(installation_id) {
            return Ok(t);
        }

        let cached = self.minter.mint(installation_id).await?;
        let token = cached.token.clone();
        self.inner.insert(installation_id, cached);
        Ok(token)
    }

    fn read_fresh(&self, installation_id: i64) -> Option<Secret<String>> {
        let entry = self.inner.get(&installation_id)?;
        let remaining = entry.expires_at.signed_duration_since(Utc::now());
        if remaining
            > chrono::Duration::from_std(SAFETY_MARGIN).expect("safety margin fits in chrono")
        {
            Some(entry.token.clone())
        } else {
            None
        }
    }

    /// Drops cache entries whose tokens are past expiry. Cheap; intended
    /// for the periodic sweep but safe to call at any time.
    pub fn evict_expired(&self) {
        let now = Utc::now();
        self.inner.retain(|_, t| t.expires_at > now);
    }

    /// Spawns a periodic eviction task. Call once at startup. The task
    /// holds only a `Weak` reference to `self`, so the cache can still be
    /// dropped; the task observes the drop on its next tick and exits.
    /// Calling this more than once aborts the previous handle.
    pub fn start_sweep(self: &Arc<Self>) {
        self.start_sweep_with_period(SWEEP_INTERVAL);
    }

    fn start_sweep_with_period(self: &Arc<Self>, period: Duration) {
        let weak = Arc::downgrade(self);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(period);
            // First tick fires immediately; skip it so the sweep happens
            // one period after startup.
            interval.tick().await;
            loop {
                interval.tick().await;
                let Some(strong) = weak.upgrade() else {
                    break;
                };
                strong.evict_expired();
            }
        });
        if let Ok(mut slot) = self.sweep.lock() {
            if let Some(prev) = slot.replace(handle) {
                prev.abort();
            }
        }
    }

    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.inner.len()
    }
}

impl std::fmt::Debug for TokenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenCache")
            .field("entries", &self.inner.len())
            .finish_non_exhaustive()
    }
}

impl Drop for TokenCache {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.sweep.lock() {
            if let Some(handle) = slot.take() {
                handle.abort();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    /// Test minter that returns a token with caller-controlled expiry and
    /// counts how many times it was invoked.
    struct CountingMinter {
        calls: AtomicUsize,
        expires_in: chrono::Duration,
        delay: Option<Duration>,
    }

    impl CountingMinter {
        fn new(expires_in: chrono::Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                expires_in,
                delay: None,
            })
        }

        fn with_delay(expires_in: chrono::Duration, delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                expires_in,
                delay: Some(delay),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl TokenMinter for CountingMinter {
        fn mint(&self, installation_id: i64) -> MintFuture<'_> {
            Box::pin(async move {
                if let Some(d) = self.delay {
                    tokio::time::sleep(d).await;
                }
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                Ok(CachedToken {
                    token: Secret::new(format!("ghs_inst_{installation_id}_call_{n}")),
                    expires_at: Utc::now() + self.expires_in,
                })
            })
        }
    }

    #[tokio::test]
    async fn warm_hit_returns_cached_token_without_minting() {
        let minter = CountingMinter::new(chrono::Duration::minutes(60));
        let cache = TokenCache::new(minter.clone());

        let first = cache.get_or_mint(42).await.unwrap();
        let second = cache.get_or_mint(42).await.unwrap();

        assert_eq!(minter.calls(), 1, "second call must hit cache");
        assert_eq!(first.expose(), second.expose());
    }

    #[tokio::test]
    async fn token_within_safety_margin_triggers_remint() {
        let minter = CountingMinter::new(chrono::Duration::minutes(5));
        let cache = TokenCache::new(minter.clone());

        let _ = cache.get_or_mint(42).await.unwrap();
        let _ = cache.get_or_mint(42).await.unwrap();

        assert_eq!(
            minter.calls(),
            2,
            "tokens with <10 min remaining must be re-minted on lookup",
        );
    }

    #[tokio::test]
    async fn token_with_exactly_safety_margin_remints() {
        // 10 min remaining means `remaining > 10 min` is false → re-mint.
        let minter = CountingMinter::new(chrono::Duration::minutes(10));
        let cache = TokenCache::new(minter.clone());

        let _ = cache.get_or_mint(42).await.unwrap();
        let _ = cache.get_or_mint(42).await.unwrap();

        assert_eq!(minter.calls(), 2);
    }

    #[tokio::test]
    async fn distinct_installations_mint_independently() {
        let minter = CountingMinter::new(chrono::Duration::minutes(60));
        let cache = TokenCache::new(minter.clone());

        let _ = cache.get_or_mint(1).await.unwrap();
        let _ = cache.get_or_mint(2).await.unwrap();
        let _ = cache.get_or_mint(1).await.unwrap();

        assert_eq!(minter.calls(), 2, "one mint per installation");
    }

    #[tokio::test]
    async fn single_flight_collapses_concurrent_cold_misses() {
        let minter =
            CountingMinter::with_delay(chrono::Duration::minutes(60), Duration::from_millis(50));
        let cache = TokenCache::new(minter.clone());

        let mut handles = Vec::with_capacity(100);
        for _ in 0..100 {
            let c = cache.clone();
            handles.push(tokio::spawn(async move { c.get_or_mint(42).await }));
        }
        for h in handles {
            h.await.unwrap().unwrap();
        }

        assert_eq!(
            minter.calls(),
            1,
            "single-flight must collapse concurrent cold-misses",
        );
    }

    #[tokio::test]
    async fn cache_hit_is_sub_millisecond() {
        let minter = CountingMinter::new(chrono::Duration::minutes(60));
        let cache = TokenCache::new(minter.clone());

        cache.get_or_mint(42).await.unwrap();

        let start = Instant::now();
        for _ in 0..1_000 {
            cache.get_or_mint(42).await.unwrap();
        }
        let avg = start.elapsed() / 1_000;

        assert_eq!(minter.calls(), 1);
        assert!(
            avg < Duration::from_millis(1),
            "average warm-path lookup was {avg:?}, expected < 1 ms",
        );
    }

    #[tokio::test]
    async fn evict_expired_drops_stale_entries() {
        let minter = CountingMinter::new(chrono::Duration::milliseconds(50));
        let cache = TokenCache::new(minter.clone());

        cache.get_or_mint(1).await.unwrap();
        assert_eq!(cache.entry_count(), 1);

        tokio::time::sleep(Duration::from_millis(75)).await;
        cache.evict_expired();
        assert_eq!(cache.entry_count(), 0);
    }

    #[tokio::test]
    async fn sweep_task_runs_and_aborts_on_drop() {
        let minter = CountingMinter::new(chrono::Duration::milliseconds(20));
        let cache = TokenCache::new(minter.clone());
        cache.start_sweep_with_period(Duration::from_millis(30));

        cache.get_or_mint(7).await.unwrap();
        assert_eq!(cache.entry_count(), 1);

        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(cache.entry_count(), 0, "sweep should evict expired entry");

        drop(cache);
    }
}
