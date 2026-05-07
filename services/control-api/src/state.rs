use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64, Ordering},
};

use crate::crypto::OauthTokenCipher;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rb_auth::{LoginRateLimiter, PasswordHasher};
pub use rb_mcp::McpSessionStore;
use rb_email::EmailSender;
use rb_github::GhApp;
use rb_kafka::Producer;
use rb_query::ModuleTreeCache;
use rb_schemas::{IngestRequest, Tombstone};
use rb_sse::EventBus;
use rb_storage_neo4j::TenantGraph;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::PgPool;
use tokio::sync::Semaphore;
use uuid::Uuid;

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
///
/// - Tracks all running sessions with their token budgets.
/// - Enforces `MAX_ACTIVE_SESSIONS_PER_PROCESS` via a semaphore.
/// - Cheap to clone (Arc-backed).
#[derive(Clone)]
pub struct AgentRegistry {
    sessions: Arc<DashMap<Uuid, SessionHandle>>,
    /// Semaphore cap at `MAX_ACTIVE_SESSIONS_PER_PROCESS`.
    semaphore: Arc<Semaphore>,
    /// Global token budget meter (sum of tokens used across all live sessions).
    global_tokens_used: Arc<AtomicI64>,
}

impl AgentRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            semaphore: Arc::new(Semaphore::new(MAX_ACTIVE_SESSIONS_PER_PROCESS)),
            global_tokens_used: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Try to acquire a slot.  Returns `None` if at the process cap.
    #[must_use]
    pub fn try_acquire(&self) -> Option<tokio::sync::SemaphorePermit<'_>> {
        self.semaphore.try_acquire().ok()
    }

    /// Insert a new session handle (call after acquiring semaphore slot).
    pub fn insert(&self, handle: SessionHandle) {
        self.sessions.insert(handle.session_id, handle);
    }

    /// Remove a completed/failed session.
    #[must_use]
    pub fn remove(&self, session_id: &Uuid) -> Option<SessionHandle> {
        self.sessions.remove(session_id).map(|(_, h)| h)
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
    /// In-process MCP session table (ADR-009 Phase 1).
    pub mcp_sessions: McpSessionStore,
    /// In-process agent session registry (ADR-009 Phase 1).
    pub agent_registry: AgentRegistry,
    /// AES-256-GCM cipher for OAuth token encryption (RUSAA-862).
    /// `None` when `RB_OAUTH_ENCRYPT_KEY` is not configured (development only).
    pub token_cipher: Option<Arc<OauthTokenCipher>>,
    /// Previous-key cipher used during a rotation window.
    /// `None` when `RB_OAUTH_ENCRYPT_KEY_PREV` is not configured.
    pub token_cipher_prev: Option<Arc<OauthTokenCipher>>,
}
