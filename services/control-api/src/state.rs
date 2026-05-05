use std::sync::Arc;

use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::EmailSender;
use rb_github::GhApp;
use rb_kafka::Producer;
use rb_query::ModuleTreeCache;
use rb_schemas::IngestRequest;
use rb_sse::EventBus;
use sqlx::PgPool;

use crate::config::Config;

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
    /// 60-second in-process cache for `GET /v1/repos/{id}/modules` (ADR-008 §3.6 / AC3).
    /// Keyed by `(repo_id, last_succeeded_ingest_run_id)`.
    pub module_tree_cache: ModuleTreeCache,
}
