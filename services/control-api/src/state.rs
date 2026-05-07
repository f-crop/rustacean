use std::sync::{
    Arc,
    atomic::{AtomicI64, AtomicU64},
};

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
}
