//! Agent session runner — spawns a tokio task to execute the runtime (ADR-009 §6.4).
//!
//! Each session gets a dedicated async task that:
//! 1. Builds the appropriate `AgentRuntime` adapter (`ClaudeCode`, `OpenCode`, `Pi`)
//! 2. Runs the LLM loop with tool dispatch
//! 3. Persists events to `agent_events` table
//! 4. Publishes events to the SSE bus for live streaming
//! 5. Updates session status on completion/failure

use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use chrono::Utc;
use rb_agent_runtime::{
    AgentRuntime, EventEnvelope, SessionContext, SessionEvent,
};
use rb_schemas::TenantId;
use sqlx::PgPool;
use tracing::instrument;
use uuid::Uuid;

use crate::{
    agents::tool_dispatch::ControlApiToolDispatch,
    state::{AgentRegistry, SessionHandle},
};

/// Spawn a session execution task.
///
/// Called by `create_session` after the session row is inserted and the handle
/// is registered. Returns immediately; the spawned task drives the session to
/// completion.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::needless_pass_by_value)] // handle is moved into the async task
pub fn spawn_session_runner(
    pool: PgPool,
    registry: AgentRegistry,
    sse_bus: Arc<rb_sse::EventBus>,
    handle: SessionHandle,
    runtime_kind: String,
    model: String,
    system_prompt: String,
    initial_message: String,
    token_budget: i64,
    // Runtime-specific dependencies
    http: reqwest::Client,
    token_store: Option<Arc<dyn rb_agent_runtime::TokenStore>>,
    litellm_url: Option<String>,
    litellm_key: Option<String>,
    tool_dispatch: ControlApiToolDispatch,
) {
    tokio::spawn(async move {
        let session_id = handle.session_id;
        let tenant_id = handle.tenant_id;

        tracing::info!(%session_id, %runtime_kind, "starting agent session runner");

        // Build the runtime adapter based on runtime_kind
        let Some(runtime): Option<Box<dyn AgentRuntime>> = build_runtime(
            &runtime_kind,
            http,
            token_store,
            litellm_url,
            litellm_key,
        ) else {
            tracing::error!(%session_id, %runtime_kind, "failed to build runtime adapter");
            let _ = update_session_failed(
                &pool,
                session_id,
                "runtime_unavailable",
                "Runtime adapter not configured",
            )
            .await;
            let _ = registry.remove(&session_id);
            return;
        };

        // Construct session context
        let ctx = SessionContext {
            session_id,
            tenant_id,
            user_id: handle.user_id,
            model,
            system_prompt,
            initial_message,
            token_budget,
        };

        // Track sequence number for event ordering (atomic to allow Fn closure access)
        let sequence = Arc::new(AtomicI64::new(0));

        // Run the session with event callback
        let result = runtime
            .run(
                ctx,
                &tool_dispatch,
                &|event: SessionEvent| {
                    let seq = sequence.fetch_add(1, Ordering::SeqCst) + 1;
                    let env = EventEnvelope::new(session_id, tenant_id, seq, &event);

                    // Persist to DB (fire and forget — log error but don't stop session)
                    let pool_clone = pool.clone();
                    let event_for_db = event.clone();
                    tokio::spawn(async move {
                        if let Err(e) = persist_event(&pool_clone, session_id, tenant_id, seq, &event_for_db).await {
                            tracing::warn!(%session_id, "failed to persist event: {e}");
                        }
                    });

                    // Publish to SSE bus
                    let tenant = TenantId::from(tenant_id);
                    sse_bus.publish(&tenant, "agent.event", &env);
                },
            )
            .await;

        // Handle completion
        match result {
            Ok(outcome) => {
                tracing::info!(
                    %session_id,
                    tokens_used = outcome.tokens_used,
                    "agent session completed"
                );
                if let Err(e) = update_session_completed(&pool, session_id, outcome.tokens_used).await {
                    tracing::warn!(%session_id, "failed to update session completion: {e}");
                }
            }
            Err(e) => {
                tracing::error!(%session_id, error = %e, "agent session failed");
                let (error_kind, error_msg) = map_runtime_error(&e);
                if let Err(e) = update_session_failed(&pool, session_id, error_kind, &error_msg).await {
                    tracing::warn!(%session_id, "failed to update session failure: {e}");
                }
            }
        }

        // Clean up registry
        let _ = registry.remove(&session_id);
    });
}

    /// Build the appropriate runtime adapter based on `runtime_kind`.
fn build_runtime(
    runtime_kind: &str,
    http: reqwest::Client,
    token_store: Option<Arc<dyn rb_agent_runtime::TokenStore>>,
    litellm_url: Option<String>,
    litellm_key: Option<String>,
) -> Option<Box<dyn AgentRuntime>> {
    match runtime_kind {
        "claude_code" => {
            let store = token_store?;
            Some(Box::new(rb_agent_runtime::ClaudeCodeRuntime::new(
                http, store,
            )))
        }
        "open_code" => {
            let url = litellm_url?;
            let key = litellm_key?;
            Some(Box::new(rb_agent_runtime::OpenCodeRuntime::new(
                http, url, key,
            )))
        }
        "pi" => {
            let url = litellm_url?;
            let key = litellm_key?;
            Some(Box::new(rb_agent_runtime::PiRuntime::new(http, url, key)))
        }
        _ => None,
    }
}

/// Persist a session event to the database.
#[instrument(skip(pool, event), fields(session_id = %session_id))]
async fn persist_event(
    pool: &PgPool,
    session_id: Uuid,
    tenant_id: Uuid,
    sequence: i64,
    event: &SessionEvent,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        INSERT INTO agents.agent_events
            (id, session_id, tenant_id, event_type, sequence, payload, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ",
    )
    .bind(Uuid::new_v4())
    .bind(session_id)
    .bind(tenant_id)
    .bind(event.event_type())
    .bind(sequence)
    .bind(serde_json::to_value(event).unwrap_or_default())
    .bind(Utc::now())
    .execute(pool)
    .await?;

    Ok(())
}

/// Update session status to completed.
async fn update_session_completed(
    pool: &PgPool,
    session_id: Uuid,
    tokens_used: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE agents.agent_sessions
        SET status = 'completed',
            tokens_used = $1,
            completed_at = $2
        WHERE id = $3
        ",
    )
    .bind(tokens_used)
    .bind(Utc::now())
    .bind(session_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Update session status to failed.
async fn update_session_failed(
    pool: &PgPool,
    session_id: Uuid,
    error_kind: &str,
    error_message: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r"
        UPDATE agents.agent_sessions
        SET status = 'failed',
            failed_at = $1,
            failure_reason = $2
        WHERE id = $3
        ",
    )
    .bind(Utc::now())
    .bind(format!("{error_kind}: {error_message}"))
    .bind(session_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Map runtime errors to (kind, message) for persistence.
fn map_runtime_error(e: &rb_agent_runtime::RuntimeError) -> (&'static str, String) {
    use rb_agent_runtime::RuntimeError;

    match e {
        RuntimeError::Cancelled => ("cancelled", "Session cancelled by user".to_owned()),
        RuntimeError::BudgetExhausted { used, budget } => (
            "budget_exhausted",
            format!("Token budget exhausted: {used}/{budget}"),
        ),
        RuntimeError::AnthropicApi { status, message } => (
            "llm_api_error",
            format!("Anthropic API error {status}: {message}"),
        ),
        RuntimeError::LiteLlmApi { status, message } => (
            "llm_api_error",
            format!("LiteLLM API error {status}: {message}"),
        ),
        RuntimeError::TokenMissing { runtime_kind } => (
            "oauth_required",
            format!("OAuth token missing for {runtime_kind}"),
        ),
        RuntimeError::Http(e) => ("network_error", e.to_string()),
        RuntimeError::Serde(e) => ("serialization_error", e.to_string()),
        RuntimeError::Internal(msg) => ("internal_error", msg.clone()),
        RuntimeError::ToolDispatch(msg) => ("tool_dispatch_error", msg.clone()),
    }
}
