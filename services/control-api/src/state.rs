use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::EmailSender;
use rb_github::GhApp;
use rb_kafka::Producer;
use rb_query::ModuleTreeCache;
use rb_schemas::{AgentSessionCommand, IngestRequest, Tombstone};
use rb_sse::EventBus;
use rb_storage_neo4j::TenantGraph;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::PgPool;
use uuid::Uuid;

pub use rb_mcp::McpSessionStore;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Kafka consistency state
// ---------------------------------------------------------------------------

/// Shared in-memory Kafka consistency state, updated by `ingest_consumer` on
/// each consumed message and read by `GET /v1/health/consistency` (REQ-DP-07).
pub struct KafkaConsistencyState {
    /// Unix epoch milliseconds of the last consumed event; 0 means never.
    pub last_event_at_ms: AtomicI64,
    /// Number of messages in the consumer lag window.
    pub lag_records: AtomicU64,
}

impl KafkaConsistencyState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_event_at_ms: AtomicI64::new(0),
            lag_records: AtomicU64::new(0),
        }
    }
}

impl Default for KafkaConsistencyState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// SessionCreateRateLimiter — REQ-MC-02
// ---------------------------------------------------------------------------

/// Default: 10 session-creates per tenant per minute (REQ-MC-02).
pub const DEFAULT_SESSION_CREATE_RATE_LIMIT: usize = 10;

/// Default: 60-second sliding window for session creation (REQ-MC-02).
pub const DEFAULT_SESSION_CREATE_WINDOW_SECS: u64 = 60;

/// Default: 100 active sessions max per tenant (REQ-MC-02).
#[allow(dead_code)]
pub const DEFAULT_TENANT_SESSION_CAP: usize = 100;

/// In-memory sliding-window rate limiter for agent session creation.
///
/// Tracks creation attempts per tenant. After `max_creates` creations in a
/// `window_secs`-second window, further creates are rejected with 429.
///
/// The `check_and_record` method atomically checks and records an attempt,
/// preventing TOCTOU races between concurrent requests from the same tenant.
///
/// Suitable for single-instance deployments. Multi-instance setups would
/// need a Redis-backed implementation behind the same interface.
#[derive(Clone)]
pub struct SessionCreateRateLimiter {
    /// Tenant ID → timestamps of session creation attempts within the window.
    windows: Arc<DashMap<Uuid, Vec<Instant>>>,
    /// Maximum session creations allowed per tenant in the sliding window.
    pub max_creates: usize,
    /// Sliding window duration in seconds.
    pub window_secs: u64,
}

impl SessionCreateRateLimiter {
    /// Create a new rate limiter with the given limits.
    #[must_use]
    pub fn new(max_creates: usize, window_secs: u64) -> Self {
        Self {
            windows: Arc::new(DashMap::new()),
            max_creates,
            window_secs,
        }
    }

    /// Atomically check whether a tenant is rate-limited and record the attempt.
    ///
    /// If the tenant has not exceeded the threshold, the attempt is recorded
    /// and `Ok(())` is returned. If the threshold is exceeded, returns
    /// `Err(retry_after_secs)` without recording.
    ///
    /// This single-method design prevents TOCTOU races that would occur with
    /// separate `check()` + `record()` calls.
    ///
    /// # Errors
    ///
    /// Returns `Err(retry_after_secs)` when the tenant has exceeded its rate
    /// limit within the current window.
    pub fn check_and_record(&self, tenant_id: Uuid) -> Result<(), u64> {
        let now = Instant::now();
        let window = Duration::from_secs(self.window_secs);

        let mut entry = self.windows.entry(tenant_id).or_default();
        entry.retain(|t| now.duration_since(*t) < window);

        if entry.len() >= self.max_creates {
            let oldest = entry.iter().copied().min().unwrap_or(now);
            let elapsed = now.duration_since(oldest).as_secs();
            let retry_after = self.window_secs.saturating_sub(elapsed);
            Err(retry_after.max(1))
        } else {
            entry.push(now);
            Ok(())
        }
    }
}

impl Default for SessionCreateRateLimiter {
    fn default() -> Self {
        Self::new(
            DEFAULT_SESSION_CREATE_RATE_LIMIT,
            DEFAULT_SESSION_CREATE_WINDOW_SECS,
        )
    }
}

// ---------------------------------------------------------------------------
// TenantSessionCount — per-tenant active session cap (REQ-MC-02)
// ---------------------------------------------------------------------------

/// Tracks active session counts per tenant for the per-tenant session cap.
///
/// **Known limitation (single-instance in-memory design):** The counter starts
/// at zero on each process restart. Active sessions that were created before
/// the restart and are still running do not pre-populate the counter, so the
/// effective cap is temporarily higher than configured until those legacy
/// sessions terminate naturally. This is acceptable for the current
/// single-instance deployment; a multi-instance deployment would require
/// querying the DB at startup or using a shared store (e.g. Redis).
#[derive(Default)]
pub struct TenantSessionCount {
    counts: DashMap<Uuid, AtomicUsize>,
}

impl TenantSessionCount {
    /// Create a new tenant session counter.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attempt to increment the session count for a tenant.
    ///
    /// Returns `false` if the tenant is already at or above `max_sessions`.
    /// Uses atomic compare-and-swap for thread safety.
    #[must_use]
    pub fn try_increment(&self, tenant_id: &Uuid, max_sessions: usize) -> bool {
        let count = self.counts.entry(*tenant_id).or_insert(AtomicUsize::new(0));
        loop {
            let current = count.load(Ordering::Relaxed);
            if current >= max_sessions {
                return false;
            }
            if count
                .compare_exchange_weak(current, current + 1, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return true;
            }
        }
    }

    /// Decrement the session count for a tenant.
    pub fn decrement(&self, tenant_id: &Uuid) {
        if let Some(count) = self.counts.get(tenant_id) {
            let prev = count.fetch_sub(1, Ordering::Relaxed);
            // Clean up zero-count entries to avoid unbounded map growth.
            if prev <= 1 {
                drop(count);
                self.counts
                    .remove_if(tenant_id, |_, v| v.load(Ordering::Relaxed) == 0);
            }
        }
    }

    /// Get the current active session count for a tenant.
    #[must_use]
    pub fn count(&self, tenant_id: &Uuid) -> usize {
        self.counts
            .get(tenant_id)
            .map_or(0, |c| c.load(Ordering::Relaxed))
    }
}

// ---------------------------------------------------------------------------
// AgentRegistry — ADR-009 Phase 1
// ---------------------------------------------------------------------------

/// Per-process concurrency limit for active agent sessions (ADR-009 §3.2).
pub const MAX_ACTIVE_SESSIONS_PER_PROCESS: usize = 200;

/// Live-session entry tracked in the in-memory registry.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub runtime_kind: String,
    pub token_budget: i64,
    pub tokens_used: Arc<AtomicI64>,
    pub status: Arc<tokio::sync::RwLock<String>>,
    pub created_at: DateTime<Utc>,
}

impl SessionHandle {
    pub fn new(
        session_id: Uuid,
        tenant_id: Uuid,
        user_id: Uuid,
        runtime_kind: String,
        token_budget: i64,
    ) -> Self {
        Self {
            session_id,
            tenant_id,
            user_id,
            runtime_kind,
            token_budget,
            tokens_used: Arc::new(AtomicI64::new(0)),
            status: Arc::new(tokio::sync::RwLock::new("created".to_owned())),
            created_at: Utc::now(),
        }
    }

    pub fn add_tokens(&self, n: i64) {
        self.tokens_used.fetch_add(n, Ordering::Relaxed);
    }

    pub fn tokens_used(&self) -> i64 {
        self.tokens_used.load(Ordering::Relaxed)
    }

    pub fn budget_remaining(&self) -> i64 {
        self.token_budget - self.tokens_used()
    }
}

/// In-memory registry of active agent sessions.
#[derive(Clone)]
pub struct AgentRegistry {
    sessions: Arc<DashMap<Uuid, SessionHandle>>,
    active_count_atomic: Arc<AtomicUsize>,
    global_tokens_used: Arc<AtomicI64>,
}

impl AgentRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            active_count_atomic: Arc::new(AtomicUsize::new(0)),
            global_tokens_used: Arc::new(AtomicI64::new(0)),
        }
    }

    #[must_use]
    #[allow(clippy::single_match)]
    pub fn try_increment(&self) -> bool {
        loop {
            let current = self.active_count_atomic.load(Ordering::Relaxed);
            if current >= MAX_ACTIVE_SESSIONS_PER_PROCESS {
                return false;
            }
            match self.active_count_atomic.compare_exchange_weak(
                current,
                current + 1,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(_) => {}
            }
        }
    }

    fn decrement(&self) {
        self.active_count_atomic.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn insert(&self, handle: SessionHandle) {
        self.sessions.insert(handle.session_id, handle);
    }

    #[must_use]
    pub fn remove(&self, session_id: &Uuid) -> Option<SessionHandle> {
        let result = self.sessions.remove(session_id).map(|(_, h)| h);
        if result.is_some() {
            self.decrement();
        }
        result
    }

    /// Return a clone of the handle for the given session, if active.
    #[must_use]
    pub fn get(&self, session_id: &Uuid) -> Option<SessionHandle> {
        self.sessions.get(session_id).map(|r| r.clone())
    }

    /// Number of currently active sessions in this process.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.sessions.len()
    }

    /// Accumulate tokens used into the global meter.
    pub fn record_tokens(&self, n: i64) {
        self.global_tokens_used.fetch_add(n, Ordering::Relaxed);
    }

    /// Total tokens consumed across all live sessions since process start.
    #[must_use]
    pub fn global_tokens_used(&self) -> i64 {
        self.global_tokens_used.load(Ordering::Relaxed)
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

/// Shared application state injected into every request handler.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub email_sender: Arc<dyn EmailSender>,
    pub hasher: Arc<PasswordHasher>,
    pub login_rate_limiter: Arc<LoginRateLimiter>,
    pub config: Arc<Config>,
    /// Cached copy of `RB_INTERNAL_SECRET` for internal endpoint auth.
    pub internal_secret: String,
    /// GitHub App handle. `None` when `RB_GH_APP_ID` / `RB_GH_APP_PRIVATE_KEY`
    /// are not configured; GitHub routes return 503 in that case.
    pub gh: Option<Arc<GhApp>>,
    /// SSE event bus — per-tenant live event fan-out for `GET /v1/ingest/events`.
    pub sse_bus: Arc<EventBus>,
    /// Kafka producer for `rb.ingest.clone.commands`. `None` when Kafka is not
    /// reachable; `POST /v1/repos/{id}/ingestions` returns 503 in that case.
    pub ingest_producer: Option<Arc<Producer<IngestRequest>>>,
    /// Kafka producer for `rb.tombstones.v1`. `None` when Kafka is not reachable;
    /// `DELETE /v1/tenants/{id}` returns 503 in that case (REQ-TN-04).
    pub tombstone_producer: Option<Arc<Producer<Tombstone>>>,
    /// 60-second in-process cache for `GET /v1/repos/{id}/modules` (ADR-008 §3.6 / AC3).
    /// Keyed by `(repo_id, last_succeeded_ingest_run_id)`.
    pub module_tree_cache: ModuleTreeCache,
    /// Neo4j tenant-graph handle.  `None` when `RB_NEO4J_URI` is not configured;
    /// graph endpoints (`/impls`, `/usages`) return 503 in that case.
    pub graph: Option<Arc<TenantGraph>>,
    /// Qdrant vector store for semantic search (REQ-DP-01). `None` when
    /// `RB_QDRANT_URL` is not configured; `POST /v1/search` returns 503.
    pub qdrant: Option<Arc<TenantVectorStore>>,
    /// Shared HTTP client for outbound health probes (Qdrant, Ollama, etc.).
    pub http_client: reqwest::Client,
    /// Neo4j bolt URI for TCP health probe (REQ-DP-07). `None` when not configured.
    pub neo4j_uri: Option<String>,
    /// Kafka consistency state updated by `ingest_consumer` on each consumed message.
    pub kafka_consistency: Arc<KafkaConsistencyState>,
    /// In-process MCP session table (kept for compat — will be removed).
    pub mcp_sessions: McpSessionStore,
    /// In-process agent session registry — semaphore enforces per-process cap.
    pub agent_registry: AgentRegistry,
    /// Kafka producer for `rb.agent.commands`. `None` when Kafka is not reachable.
    pub agent_commands_producer: Option<Arc<Producer<AgentSessionCommand>>>,
    /// Per-tenant sliding-window rate limiter for session creation (REQ-MC-02).
    pub session_create_rate_limiter: Arc<SessionCreateRateLimiter>,
    /// Per-tenant active session counter (REQ-MC-02 tenant cap).
    pub tenant_session_count: Arc<TenantSessionCount>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_rate_limiter_allows_under_limit() {
        let limiter = SessionCreateRateLimiter::new(3, 60);
        let tenant = Uuid::new_v4();
        assert!(limiter.check_and_record(tenant).is_ok());
        assert!(limiter.check_and_record(tenant).is_ok());
        assert!(limiter.check_and_record(tenant).is_ok());
    }

    #[test]
    fn session_rate_limiter_rejects_at_limit() {
        let limiter = SessionCreateRateLimiter::new(3, 60);
        let tenant = Uuid::new_v4();
        limiter.check_and_record(tenant).unwrap();
        limiter.check_and_record(tenant).unwrap();
        limiter.check_and_record(tenant).unwrap();
        assert!(limiter.check_and_record(tenant).is_err());
    }

    #[test]
    fn session_rate_limiter_multi_tenant_isolation() {
        let limiter = SessionCreateRateLimiter::new(2, 60);
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        limiter.check_and_record(t1).unwrap();
        limiter.check_and_record(t1).unwrap();
        assert!(limiter.check_and_record(t1).is_err());
        assert!(limiter.check_and_record(t2).is_ok());
        assert!(limiter.check_and_record(t2).is_ok());
        assert!(limiter.check_and_record(t2).is_err());
    }

    #[test]
    fn session_rate_limiter_window_expiry_resets() {
        let limiter = SessionCreateRateLimiter::new(2, 1);
        let tenant = Uuid::new_v4();
        limiter.check_and_record(tenant).unwrap();
        limiter.check_and_record(tenant).unwrap();
        assert!(limiter.check_and_record(tenant).is_err());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(limiter.check_and_record(tenant).is_ok());
    }

    #[test]
    fn session_rate_limiter_default_matches_constants() {
        let limiter = SessionCreateRateLimiter::default();
        assert_eq!(limiter.max_creates, DEFAULT_SESSION_CREATE_RATE_LIMIT);
        assert_eq!(limiter.window_secs, DEFAULT_SESSION_CREATE_WINDOW_SECS);
    }

    #[test]
    fn tenant_session_count_increment_decrement() {
        let counter = TenantSessionCount::new();
        let tenant = Uuid::new_v4();
        assert!(counter.try_increment(&tenant, 2));
        assert_eq!(counter.count(&tenant), 1);
        assert!(counter.try_increment(&tenant, 2));
        assert_eq!(counter.count(&tenant), 2);
        assert!(!counter.try_increment(&tenant, 2));
        counter.decrement(&tenant);
        assert_eq!(counter.count(&tenant), 1);
        assert!(counter.try_increment(&tenant, 2));
    }

    #[test]
    fn tenant_session_count_decrement_cleans_up_zero() {
        let counter = TenantSessionCount::new();
        let tenant = Uuid::new_v4();
        assert!(counter.try_increment(&tenant, 5));
        counter.decrement(&tenant);
        assert_eq!(counter.count(&tenant), 0);
        assert!(counter.counts.is_empty() || counter.counts.get(&tenant).is_none());
    }

    #[test]
    fn tenant_session_count_multi_tenant_independent() {
        let counter = TenantSessionCount::new();
        let t1 = Uuid::new_v4();
        let t2 = Uuid::new_v4();
        assert!(counter.try_increment(&t1, 1));
        assert!(!counter.try_increment(&t1, 1));
        assert!(counter.try_increment(&t2, 1));
    }
}
