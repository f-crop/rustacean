//! `PATCH /internal/agent/sessions/{id}/status` — agent-runner status callback.
//!
//! Extracted from `sessions.rs` to keep that file under the 600-line cap.
//! Handles both `agents.agent_sessions` (normal path) and `control.chat_sessions`
//! (fallback for chat sessions created via `POST /v1/chat/sessions`).

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use rb_schemas::TenantId;
use serde::Deserialize;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

use super::super::session_lifecycle::{TERMINAL_STATUSES, VALID_AGENT_STATUSES};

// ---------------------------------------------------------------------------
// Request type
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

// ---------------------------------------------------------------------------
// Lifecycle helpers
// ---------------------------------------------------------------------------

/// Map a runner-reported status to the `event_type` stored in `agents.agent_events`.
///
/// Returns `None` for statuses that do not generate a lifecycle event row
/// (e.g. `terminating`, `cancelled`, `pending`).
pub(super) fn lifecycle_event_type(status: &str) -> Option<&'static str> {
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
pub(super) fn lifecycle_event_seq(status: &str) -> i64 {
    match status {
        "failed" => i64::MIN + 1,
        "terminated" => i64::MIN + 2,
        _ => 0,
    }
}

/// Build the JSONB payload for a lifecycle event row in `agents.agent_events`.
pub(super) fn lifecycle_event_payload(req: &PatchSessionStatusRequest) -> serde_json::Value {
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

// ---------------------------------------------------------------------------
// API-key revocation helper (shared with delete_session in sessions.rs)
// ---------------------------------------------------------------------------

/// Revoke the session-scoped API key tied to `session_id`.
///
/// Idempotent: the `AND revoked_at IS NULL` predicate is a no-op if the key
/// was already revoked by a previous call or the standalone DELETE endpoint.
pub(super) async fn revoke_session_api_key(pool: &sqlx::PgPool, session_id: Uuid) {
    if let Err(e) = sqlx::query(
        "UPDATE control.api_keys SET revoked_at = now() \
         WHERE id = (SELECT api_key_id FROM agents.agent_sessions WHERE id = $1) \
         AND revoked_at IS NULL",
    )
    .bind(session_id)
    .execute(pool)
    .await
    {
        tracing::warn!(session_id = %session_id, "api_key revoke failed: {e}");
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

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
    // Chat sessions live in control.chat_sessions (migration 021); try agents table first
    // and fall back to the chat table so both session kinds share this callback path.
    let agent_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if let Some((session_tenant_id,)) = agent_row {
        if session_tenant_id != req.tenant_id {
            return Err(AppError::Unauthorized);
        }

        // Agent session path: update agents.agent_sessions, emit lifecycle event.
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
                revoke_session_api_key(&state.pool, session_id).await;
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

        return Ok(StatusCode::NO_CONTENT);
    }

    patch_chat_session_status(&state, session_id, &req).await
}

/// Chat-session fallback for [`patch_session_status`].
///
/// Called when `agents.agent_sessions` has no row for `session_id` — validates ownership
/// against `control.chat_sessions`, updates the chat status for terminal transitions, and
/// fans out the lifecycle event to the SSE bus.  No `agent_registry` / session-count
/// side-effects because chat sessions are not counted against the agent concurrency limit.
async fn patch_chat_session_status(
    state: &AppState,
    session_id: Uuid,
    req: &PatchSessionStatusRequest,
) -> Result<StatusCode, AppError> {
    let chat_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM control.chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (chat_tenant_id,) = chat_row.ok_or(AppError::NotFound)?;
    if chat_tenant_id != req.tenant_id {
        return Err(AppError::Unauthorized);
    }

    // Map agent-runner status → chat_sessions.status.
    // "terminated" → "ended"; "failed" → "failed"; others are non-terminal, skip update.
    let chat_status = match req.status.as_str() {
        "terminated" => Some("ended"),
        "failed" => Some("failed"),
        _ => None,
    };

    if let Some(cs) = chat_status {
        sqlx::query(
            "UPDATE control.chat_sessions
             SET status = $1, ended_at = now(), last_activity_at = now()
             WHERE id = $2 AND tenant_id = $3
               AND status = 'active'",
        )
        .bind(cs)
        .bind(session_id)
        .bind(req.tenant_id)
        .execute(&state.pool)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("chat session update failed: {e}")))?;
    }

    if let Some(event_type) = lifecycle_event_type(&req.status) {
        let seq = lifecycle_event_seq(&req.status);
        let payload = lifecycle_event_payload(req);

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

    Ok(StatusCode::NO_CONTENT)
}
