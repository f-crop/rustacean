//! Agent session management routes (ADR-009 Option B).
//!
//! - `POST /v1/agents/sessions`             — INSERT row + publish `SessionStart` to Kafka
//! - `DELETE /v1/agents/sessions/{id}`      — publish `SessionTerminate` to Kafka
//! - `PATCH /internal/agent/sessions/{id}/status`   — agent-runner callback to update DB
//! - `DELETE /internal/agent/sessions/{id}/api-key` — agent-runner callback to revoke key
//!
//! # Prompt security
//!
//! The full `initial_prompt` is forwarded via Kafka but **never stored
//! verbatim in the database**. Only a ≤256-char Unicode preview is persisted
//! in `input_prompt_preview` (migration 011).

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
    AgentRuntime, AgentSessionCommand, AgentSessionStart, AgentSessionTerminate,
    TenantId, agent_session_command,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum Unicode code points stored as a prompt preview in the DB.
const PROMPT_PREVIEW_MAX_CHARS: usize = 256;

/// Maximum byte length accepted for `initial_prompt` (64 KiB, per ADR-009 §4.1).
const INITIAL_MESSAGE_MAX_BYTES: usize = 64 * 1024;

/// Kafka topic for agent commands.
const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns the first ≤`PROMPT_PREVIEW_MAX_CHARS` Unicode code points of `s`.
fn prompt_preview(s: &str) -> String {
    s.chars().take(PROMPT_PREVIEW_MAX_CHARS).collect()
}

fn parse_runtime(s: &str) -> Option<AgentRuntime> {
    match s {
        "claude_code" => Some(AgentRuntime::ClaudeCode),
        "opencode" => Some(AgentRuntime::Opencode),
        "pi" => Some(AgentRuntime::Pi),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateSessionRequest {
    /// One of `"claude_code"`, `"opencode"`, `"pi"`
    pub runtime: String,
    #[serde(default)]
    pub initial_prompt: String,
    /// Optional override for workspace sub-path; defaults to `tenant_id/session_id`.
    pub workspace_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct PatchSessionStatusRequest {
    pub status: String,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// POST /v1/agents/sessions
// ---------------------------------------------------------------------------

#[utoipa::path(
    post,
    path = "/v1/agents/sessions",
    request_body = serde_json::Value,
    responses(
        (status = 202, description = "Session created, pending agent-runner pickup"),
        (status = 400, description = "Invalid runtime or fields"),
        (status = 401, description = "Authentication required"),
        (status = 429, description = "Process session cap reached"),
        (status = 503, description = "Kafka unavailable"),
    ),
    tag = "agents"
)]
pub async fn create_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<CreateSessionRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let runtime = parse_runtime(&req.runtime).ok_or(AppError::InvalidInput)?;

    if req.initial_prompt.len() > INITIAL_MESSAGE_MAX_BYTES {
        return Err(AppError::InvalidInput);
    }

    // Enforce process-level session cap.
    let _permit = state
        .agent_registry
        .try_acquire()
        .ok_or(AppError::SessionCapExceeded)?;

    let session_id = Uuid::new_v4();
    let now = Utc::now();
    let preview = prompt_preview(&req.initial_prompt);

    // Derive workspace path: <tenant_id>/<session_id>
    let workspace_rel = req
        .workspace_path
        .clone()
        .unwrap_or_else(|| format!("{}/{}", session.tenant_id, session_id));

    // Generate a session-scoped API key for the spawned process.
    let raw_key = ApiKey::generate();
    let key_hash = raw_key.hash();
    let key_str = raw_key.as_str().to_owned();
    let api_key_id = Uuid::new_v4();
    let scopes_json = serde_json::json!(["agent"]);

    drop(raw_key);

    // INSERT session-scoped API key.
    sqlx::query(
        "INSERT INTO control.api_keys \
         (id, tenant_id, key_hash, name, scopes, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(api_key_id)
    .bind(session.tenant_id)
    .bind(&key_hash)
    .bind(format!("agent-session-{session_id}"))
    .bind(&scopes_json)
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert session api_key: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })?;

    // INSERT agent_session row with status = 'pending'.
    sqlx::query(
        r"
        INSERT INTO agents.agent_sessions
            (id, tenant_id, user_id, runtime_kind, model, system_prompt,
             status, token_budget, tokens_used, input_prompt_preview,
             workspace_path, api_key_id, created_at)
        VALUES ($1, $2, $3, $4, 'n/a', '',
                'pending', 100000, 0, $5,
                $6, $7, $8)
        ",
    )
    .bind(session_id)
    .bind(session.tenant_id)
    .bind(session.user_id)
    .bind(&req.runtime)
    .bind(&preview)
    .bind(&workspace_rel)
    .bind(api_key_id)
    .bind(now)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to insert agent_session: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
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

    let tenant_id = TenantId::from(session.tenant_id);
    let envelope = EventEnvelope::new(tenant_id, command);

    if let Some(producer) = state.agent_commands_producer.as_ref() {
        producer
            .publish(TOPIC_AGENT_COMMANDS, session_id.as_bytes(), envelope)
            .await
            .map_err(|e| {
                tracing::error!("failed to publish SessionStart: {e}");
                AppError::Internal(anyhow::anyhow!("Kafka publish failed"))
            })?;
    } else {
        return Err(AppError::KafkaNotConfigured);
    }

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
        (status = 202, description = "Termination queued"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Not your session"),
        (status = 404, description = "Session not found"),
        (status = 503, description = "Kafka unavailable"),
    ),
    tag = "agents"
)]
pub async fn delete_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    // Verify the session belongs to the caller's tenant.
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT tenant_id FROM agents.agent_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (session_tenant_id,) = row.ok_or(AppError::NotFound)?;
    if session_tenant_id != session.tenant_id {
        return Err(AppError::InsufficientRole);
    }

    // Publish SessionTerminate to Kafka.
    let command = AgentSessionCommand {
        session_id: session_id.to_string(),
        command: Some(agent_session_command::Command::Terminate(
            AgentSessionTerminate {
                force: false,
                reason: "user requested".into(),
            },
        )),
    };

    let tenant_id = TenantId::from(session.tenant_id);
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

pub async fn patch_session_status(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<PatchSessionStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    sqlx::query(
        "UPDATE agents.agent_sessions
         SET status = $1, pid = $2, exit_code = $3
         WHERE id = $4",
    )
    .bind(&req.status)
    .bind(req.pid)
    .bind(req.exit_code)
    .bind(session_id)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update failed: {e}")))?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// DELETE /internal/agent/sessions/{id}/api-key  (agent-runner callback)
// ---------------------------------------------------------------------------

pub async fn delete_session_api_key(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Look up the api_key_id from the session.
    let row: Option<(Option<Uuid>,)> = sqlx::query_as(
        "SELECT api_key_id FROM agents.agent_sessions WHERE id = $1",
    )
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
mod tests {
    use super::*;

    #[test]
    fn prompt_preview_short_string_unchanged() {
        assert_eq!(prompt_preview("Hello, world!"), "Hello, world!");
    }

    #[test]
    fn prompt_preview_truncates_at_256_chars() {
        let s: String = "x".repeat(1000);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
    }

    #[test]
    fn prompt_preview_handles_multibyte_unicode() {
        let s: String = "🦀".repeat(300);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn parse_runtime_valid_values() {
        assert_eq!(parse_runtime("claude_code"), Some(AgentRuntime::ClaudeCode));
        assert_eq!(parse_runtime("opencode"), Some(AgentRuntime::Opencode));
        assert_eq!(parse_runtime("pi"), Some(AgentRuntime::Pi));
    }

    #[test]
    fn parse_runtime_invalid_returns_none() {
        assert_eq!(parse_runtime("unknown"), None);
        assert_eq!(parse_runtime(""), None);
    }

    #[test]
    fn initial_message_max_bytes_is_64kib() {
        assert_eq!(INITIAL_MESSAGE_MAX_BYTES, 65_536);
    }
}
