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
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::Utc;
use rb_auth::ApiKey;
use rb_kafka::EventEnvelope;
use rb_schemas::{
    agent_session_command, AgentSessionCommand, AgentSessionStart, AgentSessionTerminate, TenantId,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session_or_agent_key, AuthContext},
    state::{AppState, SessionHandle},
};

use super::session_lifecycle::{
    parse_runtime, prompt_preview, validate_workspace_path, TERMINAL_STATUSES, VALID_AGENT_STATUSES,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum byte length accepted for `initial_prompt` (64 KiB, per ADR-009 §4.1).
const INITIAL_MESSAGE_MAX_BYTES: usize = 64 * 1024;

/// Kafka topic for agent commands.
const TOPIC_AGENT_COMMANDS: &str = "rb.agent.commands";

// ---------------------------------------------------------------------------
// DB helpers
// ---------------------------------------------------------------------------

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

#[derive(Debug, Deserialize)]
pub struct PatchSessionStatusRequest {
    pub status: String,
    pub pid: Option<i64>,
    pub exit_code: Option<i32>,
    /// Optional error string — recorded into `failure_reason` when status="failed".
    /// Ignored for all other statuses.
    #[serde(default)]
    pub error: Option<String>,
    /// Required: `tenant_id` must match the session's tenant for authorization.
    pub tenant_id: Uuid,
}

/// Map a runner-reported status to the `event_type` stored in `agents.agent_events`.
///
/// Returns `None` for statuses that do not generate a lifecycle event row
/// (e.g. `terminating`, `cancelled`, `pending`).
fn lifecycle_event_type(status: &str) -> Option<&'static str> {
    match status {
        "running" => Some("session.running"),
        "failed" => Some("session.failed"),
        "terminated" => Some("session.completed"),
        _ => None,
    }
}

/// Canonical sequence values matching the runner's sentinel constants.
///
/// - `failed`     → `i64::MIN + 1`  (`ERROR_SEQ` in agent-runner/src/consumer.rs)
/// - `terminated` → `i64::MIN + 2`  (`TERMINATED_SEQ` in agent-runner/src/session.rs)
/// - `running`    → `0`             (Started event seq)
fn lifecycle_event_seq(status: &str) -> i64 {
    match status {
        "failed" => i64::MIN + 1,
        "terminated" => i64::MIN + 2,
        _ => 0,
    }
}

/// Build the JSONB payload for a lifecycle event row in `agents.agent_events`.
fn lifecycle_event_payload(req: &PatchSessionStatusRequest) -> serde_json::Value {
    match req.status.as_str() {
        "running" => serde_json::json!({ "pid": req.pid }),
        "failed" => serde_json::json!({
            "failure_reason": req.error,
            "exit_code": req.exit_code,
        }),
        "terminated" => serde_json::json!({ "exit_code": req.exit_code }),
        _ => serde_json::json!({}),
    }
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

    // The `status NOT IN (...)` guard makes terminal states sticky.  Without it a
    // late callback could overwrite `failed` with `running` or similar.
    let result = if req.status == "failed" {
        sqlx::query(
            "UPDATE agents.agent_sessions
             SET status = $1, pid = $2, exit_code = $3,
                 failed_at = now(),
                 failure_reason = COALESCE($6, failure_reason)
             WHERE id = $4 AND tenant_id = $5
               AND status NOT IN ('terminated', 'cancelled', 'failed', 'completed')",
        )
        .bind(&req.status)
        .bind(req.pid)
        .bind(req.exit_code)
        .bind(session_id)
        .bind(req.tenant_id)
        .bind(req.error.as_deref())
        .execute(&state.pool)
        .await
    } else {
        sqlx::query(
            "UPDATE agents.agent_sessions
             SET status = $1, pid = $2, exit_code = $3
             WHERE id = $4 AND tenant_id = $5
               AND status NOT IN ('terminated', 'cancelled', 'failed')",
        )
        .bind(&req.status)
        .bind(req.pid)
        .bind(req.exit_code)
        .bind(session_id)
        .bind(req.tenant_id)
        .execute(&state.pool)
        .await
    }
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB update failed: {e}")))?;

    if result.rows_affected() > 0 {
        if TERMINAL_STATUSES.contains(&req.status.as_str()) {
            let _ = state.agent_registry.remove(&session_id);
            state.tenant_session_count.decrement(&req.tenant_id);
        }

        if let Some(event_type) = lifecycle_event_type(&req.status) {
            let seq = lifecycle_event_seq(&req.status);
            let payload = lifecycle_event_payload(&req);
            let payload_str = payload.to_string();

            if let Err(e) = sqlx::query(
                "INSERT INTO agents.agent_events \
                 (session_id, tenant_id, event_type, sequence, payload) \
                 VALUES ($1, $2, $3, $4, $5::jsonb)",
            )
            .bind(session_id)
            .bind(req.tenant_id)
            .bind(event_type)
            .bind(seq)
            .bind(&payload_str)
            .execute(&state.pool)
            .await
            {
                tracing::warn!(
                    session_id = %session_id,
                    status = %req.status,
                    "agent_events insert failed: {e}"
                );
            }

            let tenant_id = TenantId::from(req.tenant_id);
            let sse_data = serde_json::json!({
                "session_id": session_id,
                "event_type": event_type,
                "sequence": seq,
                "payload": payload,
            });
            if let Ok(data) = serde_json::to_string(&sse_data) {
                state.sse_bus.publish_raw(&tenant_id, "session.event", data);
            }
        }
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
    fn initial_message_max_bytes_is_64kib() {
        assert_eq!(INITIAL_MESSAGE_MAX_BYTES, 65_536);
    }

    // ── lifecycle_event_type ──────────────────────────────────────────────────

    #[test]
    fn lifecycle_event_type_running() {
        assert_eq!(lifecycle_event_type("running"), Some("session.running"));
    }

    #[test]
    fn lifecycle_event_type_failed() {
        assert_eq!(lifecycle_event_type("failed"), Some("session.failed"));
    }

    #[test]
    fn lifecycle_event_type_terminated_maps_to_completed() {
        assert_eq!(
            lifecycle_event_type("terminated"),
            Some("session.completed")
        );
    }

    #[test]
    fn lifecycle_event_type_other_statuses_return_none() {
        for s in ["cancelled", "terminating", "pending", "unknown"] {
            assert_eq!(
                lifecycle_event_type(s),
                None,
                "expected None for status '{s}'"
            );
        }
    }

    // ── lifecycle_event_seq ───────────────────────────────────────────────────

    #[test]
    fn lifecycle_event_seq_failed_matches_runner_error_sentinel() {
        assert_eq!(lifecycle_event_seq("failed"), i64::MIN + 1);
    }

    #[test]
    fn lifecycle_event_seq_terminated_matches_runner_terminated_sentinel() {
        assert_eq!(lifecycle_event_seq("terminated"), i64::MIN + 2);
    }

    #[test]
    fn lifecycle_event_seq_running_is_zero() {
        assert_eq!(lifecycle_event_seq("running"), 0);
    }

    #[test]
    fn lifecycle_event_seq_sentinels_are_distinct() {
        assert_ne!(
            lifecycle_event_seq("failed"),
            lifecycle_event_seq("terminated")
        );
        assert_ne!(
            lifecycle_event_seq("failed"),
            lifecycle_event_seq("running")
        );
        assert_ne!(
            lifecycle_event_seq("terminated"),
            lifecycle_event_seq("running")
        );
    }

    // ── lifecycle_event_payload ───────────────────────────────────────────────

    #[test]
    fn lifecycle_event_payload_running_includes_pid() {
        let req = PatchSessionStatusRequest {
            status: "running".to_string(),
            pid: Some(12345),
            exit_code: None,
            error: None,
            tenant_id: Uuid::new_v4(),
        };
        let p = lifecycle_event_payload(&req);
        assert_eq!(p["pid"], 12345);
    }

    #[test]
    fn lifecycle_event_payload_failed_includes_failure_reason() {
        let req = PatchSessionStatusRequest {
            status: "failed".to_string(),
            pid: None,
            exit_code: None,
            error: Some("spawn failed: no such file".to_string()),
            tenant_id: Uuid::new_v4(),
        };
        let p = lifecycle_event_payload(&req);
        assert_eq!(p["failure_reason"], "spawn failed: no such file");
    }

    #[test]
    fn lifecycle_event_payload_failed_null_reason_when_no_error() {
        let req = PatchSessionStatusRequest {
            status: "failed".to_string(),
            pid: None,
            exit_code: None,
            error: None,
            tenant_id: Uuid::new_v4(),
        };
        let p = lifecycle_event_payload(&req);
        assert!(p["failure_reason"].is_null());
    }

    #[test]
    fn lifecycle_event_payload_terminated_includes_exit_code() {
        let req = PatchSessionStatusRequest {
            status: "terminated".to_string(),
            pid: None,
            exit_code: Some(0),
            error: None,
            tenant_id: Uuid::new_v4(),
        };
        let p = lifecycle_event_payload(&req);
        assert_eq!(p["exit_code"], 0);
    }

    #[test]
    fn lifecycle_event_payload_terminated_nonzero_exit_code() {
        let req = PatchSessionStatusRequest {
            status: "terminated".to_string(),
            pid: None,
            exit_code: Some(1),
            error: None,
            tenant_id: Uuid::new_v4(),
        };
        let p = lifecycle_event_payload(&req);
        assert_eq!(p["exit_code"], 1);
    }
}
