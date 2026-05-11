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
    AgentRuntime, AgentSessionCommand, AgentSessionStart, AgentSessionTerminate, TenantId,
    agent_session_command,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::{AppState, SessionHandle},
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

/// Statuses that agent-runner is allowed to set via the internal callback.
const VALID_AGENT_STATUSES: &[&str] = &["pending", "running", "terminated"];

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

/// Validate that `workspace_path` is a safe relative path (no `..`, no absolute).
/// Returns an error on invalid input so the session is never created.
async fn db_insert_session_api_key(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    api_key_id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    session_id: Uuid,
    key_hash: &str,
    scopes_json: &serde_json::Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO control.api_keys \
         (id, tenant_id, key_hash, name, scopes, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(api_key_id)
    .bind(tenant_id)
    .bind(key_hash)
    .bind(format!("agent-session-{session_id}"))
    .bind(scopes_json)
    .bind(user_id)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(|e| {
        tracing::error!("failed to insert session api_key: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })
}

struct NewAgentSession<'a> {
    session_id: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    runtime: &'a str,
    preview: &'a str,
    workspace_rel: &'a str,
    api_key_id: Uuid,
    now: chrono::DateTime<chrono::Utc>,
}

async fn db_insert_agent_session(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    row: &NewAgentSession<'_>,
) -> Result<(), AppError> {
    sqlx::query(
        r"INSERT INTO agents.agent_sessions
            (id, tenant_id, user_id, runtime_kind, model, system_prompt,
             status, token_budget, tokens_used, input_prompt_preview,
             workspace_path, api_key_id, created_at)
          VALUES ($1, $2, $3, $4, 'n/a', '',
                  'pending', 100000, 0, $5, $6, $7, $8)",
    )
    .bind(row.session_id)
    .bind(row.tenant_id)
    .bind(row.user_id)
    .bind(row.runtime)
    .bind(row.preview)
    .bind(row.workspace_rel)
    .bind(row.api_key_id)
    .bind(row.now)
    .execute(executor)
    .await
    .map(|_| ())
    .map_err(|e| {
        tracing::error!("failed to insert agent_session: {e}");
        AppError::Internal(anyhow::anyhow!("DB insert failed"))
    })
}

fn validate_workspace_path(path: &str) -> Result<(), AppError> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err(AppError::InvalidInput);
    }
    for component in p.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(AppError::InvalidInput);
        }
    }
    Ok(())
}

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
    let session = require_verified_session(auth)?;

    let runtime = parse_runtime(&req.runtime).ok_or(AppError::InvalidInput)?;

    if req.initial_prompt.len() > INITIAL_MESSAGE_MAX_BYTES {
        return Err(AppError::InvalidInput);
    }

    // Derive and validate workspace path before any DB writes.
    let workspace_rel = if let Some(ref path) = req.workspace_path {
        validate_workspace_path(path)?;
        path.clone()
    } else {
        // Default: tenant_id/session_id — safe by construction (UUIDs contain no `/..`).
        format!("{}/{}", session.tenant_id, Uuid::new_v4())
    };

    // C4: Acquire slot via atomic counter to ensure cap is held for session lifetime.
    if !state.agent_registry.try_increment() {
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
        session.tenant_id,
        session.user_id,
        session_id,
        &key_hash,
        &scopes_json,
    )
    .await?;

    db_insert_agent_session(
        &mut *tx,
        &NewAgentSession {
            session_id,
            tenant_id: session.tenant_id,
            user_id: session.user_id,
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

    let tenant_id = TenantId::from(session.tenant_id);
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

            return Err(AppError::Internal(anyhow::anyhow!("Kafka publish failed")));
        }
    } else {
        return Err(AppError::KafkaNotConfigured);
    }

    // Register the session in the in-memory registry so the cap is enforced
    // for the lifetime of the session, not just this request handler.
    state.agent_registry.insert(SessionHandle::new(
        session_id,
        session.tenant_id,
        session.user_id,
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
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM agents.agent_sessions WHERE id = $1")
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

#[derive(Debug, Deserialize)]
pub struct PatchSessionStatusRequest {
    pub status: String,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    /// Required: `tenant_id` must match the session's tenant for authorization.
    pub tenant_id: Uuid,
}

pub async fn patch_session_status(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<PatchSessionStatusRequest>,
) -> Result<impl IntoResponse, AppError> {
    // Reject unknown statuses to prevent arbitrary string injection into the DB.
    if !VALID_AGENT_STATUSES.contains(&req.status.as_str()) {
        return Err(AppError::InvalidInput);
    }

    // SECURITY: Verify the session belongs to the claimed tenant.
    // This prevents an attacker with the internal secret from updating arbitrary sessions.
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (session_tenant_id,) = row.ok_or(AppError::NotFound)?;
    if session_tenant_id != req.tenant_id {
        return Err(AppError::Unauthorized);
    }

    sqlx::query(
        "UPDATE agents.agent_sessions
         SET status = $1, pid = $2, exit_code = $3
         WHERE id = $4 AND tenant_id = $5",
    )
    .bind(&req.status)
    .bind(req.pid)
    .bind(req.exit_code)
    .bind(session_id)
    .bind(req.tenant_id)
    .execute(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update failed: {e}")))?;

    // Release the session slot in the registry when the process terminates.
    if req.status == "terminated" {
        let _ = state.agent_registry.remove(&session_id);
    }

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

    #[test]
    fn validate_workspace_path_rejects_traversal() {
        assert!(validate_workspace_path("../etc/passwd").is_err());
        assert!(validate_workspace_path("/absolute/path").is_err());
        assert!(validate_workspace_path("a/../../b").is_err());
        assert!(validate_workspace_path("./relative").is_err());
    }

    #[test]
    fn validate_workspace_path_accepts_valid_paths() {
        assert!(validate_workspace_path("tenant/session").is_ok());
        assert!(validate_workspace_path("abc123").is_ok());
        assert!(validate_workspace_path("tenant-id/session-id").is_ok());
    }

    #[test]
    fn valid_agent_statuses_includes_expected() {
        assert!(VALID_AGENT_STATUSES.contains(&"running"));
        assert!(VALID_AGENT_STATUSES.contains(&"terminated"));
        assert!(!VALID_AGENT_STATUSES.contains(&"unknown"));
        assert!(!VALID_AGENT_STATUSES.contains(&"'DROP TABLE'"));
    }
}
