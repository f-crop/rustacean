//! Agent session lifecycle routes (ADR-009 Option B).
//!
//! - `POST /v1/agents/sessions`             — INSERT row + publish `SessionStart` to Kafka
//! - `DELETE /v1/agents/sessions/{id}`      — publish `SessionTerminate` to Kafka
//! - `PATCH /internal/agent/sessions/{id}/status`   — agent-runner callback to update DB
//! - `DELETE /internal/agent/sessions/{id}/api-key` — agent-runner callback to revoke key
//!
//! Read-side endpoints (`GET /v1/agents/sessions`, `GET /v1/agents/sessions/{id}`)
//! live in [`super::session_queries`].
//!
//! # Prompt security
//!
//! The full `initial_prompt` is forwarded via Kafka but **never stored
//! verbatim in the database**. Only a ≤256-char Unicode preview is persisted
//! in `input_prompt_preview` (migration 011).
//!
//! # Internal endpoint security
//!
//! The `/internal/*` routes require an `X-Internal-Secret` header whose value
//! must match the `RB_INTERNAL_SECRET` environment variable.  The comparison is
//! constant-time to prevent timing attacks.  If `RB_INTERNAL_SECRET` is unset,
//! every internal request is rejected with 401.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::Utc;
use rb_auth::ApiKey;
use rb_kafka::EventEnvelope;
use rb_schemas::{
    AgentSessionCommand, AgentSessionStart, AgentSessionTerminate, TenantId, agent_session_command,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_session_or_agent_key},
    state::{AppState, SessionHandle},
};

use super::session_lifecycle::{
    TERMINAL_STATUSES, parse_runtime, prompt_preview, validate_workspace_path,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum byte length accepted for `initial_prompt` (64 KiB, per ADR-009 §4.1).
const INITIAL_MESSAGE_MAX_BYTES: usize = 64 * 1024;

/// Kafka topic for agent commands.
const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";

#[path = "sessions_db.rs"]
mod sessions_db;
use sessions_db::{NewAgentSession, db_insert_agent_session, db_insert_session_api_key};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateSessionRequest {
    /// One of `"claude_code"`, `"opencode"`, `"pi"`
    pub runtime: String,
    #[serde(default)]
    pub initial_prompt: String,
    /// Optional override for workspace sub-path; defaults to `tenant_id/session_id`.
    pub workspace_path: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub status: String,
}

// ---------------------------------------------------------------------------
// POST /v1/agents/sessions
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/v1/agents/sessions",
    request_body = CreateSessionRequest,
    responses(
        (status = 202, description = "Session created, pending agent-runner pickup"),
        (status = 400, description = "Invalid runtime or fields"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "API key lacks the `agent` scope"),
        (status = 429, description = "Process session cap reached"),
        (status = 503, description = "Kafka unavailable"),
    ),
    tag = "agents"
)]
#[allow(clippy::too_many_lines)]
pub async fn create_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_session_or_agent_key(auth)?;

    state
        .session_create_rate_limiter
        .check_and_record(caller.tenant_id)
        .map_err(|retry_after_secs| AppError::SessionRateLimitExceeded { retry_after_secs })?;

    let tenant_cap = state.config.tenant_session_cap;
    if !state
        .tenant_session_count
        .try_increment(&caller.tenant_id, tenant_cap)
    {
        return Err(AppError::TenantSessionCapExceeded);
    }

    let runtime = parse_runtime(&req.runtime).ok_or(AppError::InvalidInput)?;

    if req.initial_prompt.len() > INITIAL_MESSAGE_MAX_BYTES {
        state.tenant_session_count.decrement(&caller.tenant_id);
        return Err(AppError::InvalidInput);
    }

    let workspace_rel = if let Some(ref path) = req.workspace_path {
        if let Err(e) = validate_workspace_path(path) {
            state.tenant_session_count.decrement(&caller.tenant_id);
            return Err(e);
        }
        path.clone()
    } else {
        // Default: tenant_id/session_id — safe by construction (UUIDs contain no `/..`).
        format!("{}/{}", caller.tenant_id, Uuid::new_v4())
    };

    if !state.agent_registry.try_increment() {
        state.tenant_session_count.decrement(&caller.tenant_id);
        return Err(AppError::SessionCapExceeded);
    }

    let session_id = Uuid::new_v4();
    let now = Utc::now();
    let preview = prompt_preview(&req.initial_prompt);

    // Generate a session-scoped API key for the spawned process.
    let raw_key = ApiKey::generate();
    let key_hash = raw_key.hash();
    let key_str = raw_key.as_str().to_owned();
    let api_key_id = Uuid::new_v4();
    let scopes_json = serde_json::json!(["agent"]);

    drop(raw_key);

    // Wrap both inserts in a single transaction so a failure in the second
    // insert rolls back the first — prevents orphaned `api_keys` rows.
    let mut tx = state.pool.begin().await.map_err(|e| {
        tracing::error!("failed to start DB transaction: {e}");
        AppError::Internal(anyhow::anyhow!("TX start failed: {e}"))
    })?;

    db_insert_session_api_key(
        &mut *tx,
        api_key_id,
        caller.tenant_id,
        caller.user_id,
        session_id,
        &key_hash,
        &scopes_json,
    )
    .await?;

    db_insert_agent_session(
        &mut *tx,
        &NewAgentSession {
            session_id,
            tenant_id: caller.tenant_id,
            user_id: caller.user_id,
            runtime: &req.runtime,
            preview: &preview,
            workspace_rel: &workspace_rel,
            api_key_id,
            now,
        },
    )
    .await?;

    tx.commit().await.map_err(|e| {
        tracing::error!("failed to commit DB transaction: {e}");
        AppError::Internal(anyhow::anyhow!("TX commit failed: {e}"))
    })?;

    // Publish SessionStart command to Kafka.
    let command = AgentSessionCommand {
        session_id: session_id.to_string(),
        command: Some(agent_session_command::Command::Start(AgentSessionStart {
            runtime: runtime as i32,
            initial_prompt: req.initial_prompt.clone(),
            workspace_path: workspace_rel,
            api_key: key_str,
        })),
    };

    let tenant_id = TenantId::from(caller.tenant_id);
    let envelope = EventEnvelope::new(tenant_id, command);

    if let Some(producer) = state.agent_commands_producer.as_ref() {
        if let Err(e) = producer
            .publish(TOPIC_AGENT_COMMANDS, session_id.as_bytes(), envelope)
            .await
        {
            tracing::error!("failed to publish SessionStart: {e}");

            let _ = sqlx::query("DELETE FROM agents.agent_sessions WHERE id = $1")
                .bind(session_id)
                .execute(&state.pool)
                .await;
            let _ = sqlx::query("DELETE FROM control.api_keys WHERE id = $1")
                .bind(api_key_id)
                .execute(&state.pool)
                .await;

            state.tenant_session_count.decrement(&caller.tenant_id);

            return Err(AppError::Internal(anyhow::anyhow!("Kafka publish failed")));
        }
    } else {
        let _ = state.agent_registry.remove(&session_id);
        state.tenant_session_count.decrement(&caller.tenant_id);
        return Err(AppError::KafkaNotConfigured);
    }

    state.agent_registry.insert(SessionHandle::new(
        session_id,
        caller.tenant_id,
        caller.user_id,
        req.runtime.clone(),
        100_000,
    ));

    tracing::info!(
        session_id = %session_id,
        runtime = %req.runtime,
        "agent session created, pending agent-runner pickup"
    );

    Ok((
        StatusCode::ACCEPTED,
        Json(CreateSessionResponse {
            session_id,
            status: "pending".into(),
        }),
    ))
}

// ---------------------------------------------------------------------------
// DELETE /v1/agents/sessions/{id}
// ---------------------------------------------------------------------------

#[utoipa::path(
    delete,
    path = "/v1/agents/sessions/{id}",
    params(("id" = Uuid, Path, description = "Session ID")),
    responses(
        (status = 202, description = "Termination queued or session cancelled"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Not your session, or API key lacks the `agent` scope"),
        (status = 404, description = "Session not found"),
        (status = 503, description = "Kafka unavailable"),
    ),
    tag = "agents"
)]
#[allow(clippy::type_complexity)]
pub async fn delete_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_session_or_agent_key(auth)?;

    let row: Option<(
        Uuid,
        String,
        Option<i32>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> = sqlx::query_as(
        "SELECT tenant_id, status, pid, started_at FROM agents.agent_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (session_tenant_id, status, pid, started_at) = row.ok_or(AppError::NotFound)?;
    if session_tenant_id != caller.tenant_id {
        return Err(AppError::InsufficientRole);
    }

    if TERMINAL_STATUSES.contains(&status.as_str()) {
        let _ = state.agent_registry.remove(&session_id);
        tracing::info!(session_id = %session_id, status = %status, "delete on already-terminal session, returning 202");
        return Ok(StatusCode::ACCEPTED);
    }

    // Pending sessions with no PID/started_at can never receive a runner
    // callback, so flip to cancelled synchronously instead of enqueuing a
    // terminate command that will never be consumed.
    if status == "pending" && pid.is_none() && started_at.is_none() {
        sqlx::query(
            "UPDATE agents.agent_sessions SET status = 'cancelled', completed_at = now() WHERE id = $1"
        )
        .bind(session_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update failed: {e}")))?;

        let _ = state.agent_registry.remove(&session_id);
        sessions_patch::revoke_session_api_key(&state.pool, session_id).await;
        tracing::info!(session_id = %session_id, "pending agent session cancelled synchronously");
        return Ok(StatusCode::ACCEPTED);
    }

    let command = AgentSessionCommand {
        session_id: session_id.to_string(),
        command: Some(agent_session_command::Command::Terminate(
            AgentSessionTerminate {
                force: false,
                reason: "user requested".into(),
            },
        )),
    };

    let tenant_id = TenantId::from(caller.tenant_id);
    let envelope = EventEnvelope::new(tenant_id, command);

    if let Some(producer) = state.agent_commands_producer.as_ref() {
        producer
            .publish(TOPIC_AGENT_COMMANDS, session_id.as_bytes(), envelope)
            .await
            .map_err(|e| {
                tracing::error!("failed to publish SessionTerminate: {e}");
                AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
            })?;
    } else {
        return Err(AppError::KafkaNotConfigured);
    }

    Ok(StatusCode::ACCEPTED)
}

// ---------------------------------------------------------------------------
// PATCH /internal/agent/sessions/{id}/status  (agent-runner callback)
// ---------------------------------------------------------------------------

#[path = "sessions_patch.rs"]
mod sessions_patch;
pub use sessions_patch::patch_session_status;

// ---------------------------------------------------------------------------
// DELETE /internal/agent/sessions/{id}/api-key  (agent-runner callback)
// ---------------------------------------------------------------------------

pub async fn delete_session_api_key(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Look up the api_key_id from the session.
    let row: Option<(Option<Uuid>,)> =
        sqlx::query_as("SELECT api_key_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (api_key_id,) = row.ok_or(AppError::NotFound)?;

    if let Some(key_id) = api_key_id {
        sqlx::query(
            "UPDATE control.api_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL",
        )
        .bind(key_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("DB revoke failed: {e}")))?;
    }

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
