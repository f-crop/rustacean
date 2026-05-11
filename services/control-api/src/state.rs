use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering},
};

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
}
