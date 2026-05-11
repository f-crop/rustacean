//! Read-side endpoints for agent sessions (ADR-009 Option B).
//!
//! - `GET /v1/agents/sessions`       — list sessions for the caller's tenant
//! - `GET /v1/agents/sessions/{id}`  — get session detail
//!
//! Extracted from `sessions.rs` to keep both files under the 600-line cap.

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

/// Maximum number of sessions returned by `GET /v1/agents/sessions`.
const MAX_SESSIONS: i64 = 100;

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SessionItem {
    pub id: Uuid,
    pub runtime_kind: String,
    pub status: String,
    pub input_prompt_preview: String,
    pub workspace_path: String,
    pub tokens_used: i64,
    pub token_budget: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionItem>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct SessionDetail {
    pub id: Uuid,
    pub runtime_kind: String,
    pub status: String,
    pub input_prompt_preview: String,
    pub workspace_path: String,
    pub tokens_used: i64,
    pub token_budget: i64,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub pid: Option<i32>,
    pub exit_code: Option<i32>,
    pub failed_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
}

type SessionRow = (
    Uuid,
    String,
    String,
    String,
    String,
    i64,
    i64,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
);

type SessionDetailRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    String,
    i64,
    i64,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    Option<i32>,
    Option<i32>,
    Option<DateTime<Utc>>,
    Option<String>,
);

// ---------------------------------------------------------------------------
// GET /v1/agents/sessions
// ---------------------------------------------------------------------------

/// List all agent sessions for the current session's tenant.
///
/// Returns sessions ordered by `created_at DESC`.
/// Requires an active session.
#[utoipa::path(
    get,
    path = "/v1/agents/sessions",
    responses(
        (status = 200, description = "List of agent sessions", body = ListSessionsResponse),
        (status = 401, description = "Not authenticated"),
    ),
    tag = "agents"
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let rows: Vec<SessionRow> = sqlx::query_as(
        "SELECT id, runtime_kind, status, input_prompt_preview, workspace_path,
                tokens_used, token_budget, created_at, started_at, completed_at
         FROM agents.agent_sessions
         WHERE tenant_id = $1
         ORDER BY created_at DESC
         LIMIT $2",
    )
    .bind(session.tenant_id)
    .bind(MAX_SESSIONS)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let sessions = rows
        .into_iter()
        .map(
            |(
                id,
                runtime_kind,
                status,
                input_prompt_preview,
                workspace_path,
                tokens_used,
                token_budget,
                created_at,
                started_at,
                completed_at,
            )| SessionItem {
                id,
                runtime_kind,
                status,
                input_prompt_preview,
                workspace_path,
                tokens_used,
                token_budget,
                created_at,
                started_at,
                completed_at,
            },
        )
        .collect();

    Ok(Json(ListSessionsResponse { sessions }))
}

// ---------------------------------------------------------------------------
// GET /v1/agents/sessions/{id}
// ---------------------------------------------------------------------------

/// Get a single agent session by ID.
///
/// Performs a two-step lookup (matching `delete_session`'s pattern) so that
/// a cross-tenant access attempt yields 403 instead of 404. The query selects
/// by `id` only; tenant ownership is then verified in application code.
#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}",
    params(("id" = Uuid, Path, description = "Session ID")),
    responses(
        (status = 200, description = "Session details", body = SessionDetail),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Session belongs to another tenant"),
        (status = 404, description = "Session not found"),
    ),
    tag = "agents"
)]
pub async fn get_session(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(session_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let row: Option<SessionDetailRow> = sqlx::query_as(
        "SELECT id, tenant_id, runtime_kind, status, input_prompt_preview, workspace_path,
                tokens_used, token_budget, created_at, started_at, completed_at,
                pid, exit_code, failed_at, failure_reason
         FROM agents.agent_sessions
         WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (
        id,
        row_tenant_id,
        runtime_kind,
        status,
        input_prompt_preview,
        workspace_path,
        tokens_used,
        token_budget,
        created_at,
        started_at,
        completed_at,
        pid,
        exit_code,
        failed_at,
        failure_reason,
    ) = row.ok_or(AppError::NotFound)?;

    if row_tenant_id != session.tenant_id {
        return Err(AppError::InsufficientRole);
    }

    Ok(Json(SessionDetail {
        id,
        runtime_kind,
        status,
        input_prompt_preview,
        workspace_path,
        tokens_used,
        token_budget,
        created_at,
        started_at,
        completed_at,
        pid,
        exit_code,
        failed_at,
        failure_reason,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_sessions_is_100() {
        assert_eq!(MAX_SESSIONS, 100);
    }

    #[test]
    fn session_item_serializes_all_fields() {
        let item = SessionItem {
            id: Uuid::new_v4(),
            runtime_kind: "claude_code".to_owned(),
            status: "running".to_owned(),
            input_prompt_preview: "hello".to_owned(),
            workspace_path: "tenant/session".to_owned(),
            tokens_used: 42,
            token_budget: 100_000,
            created_at: Utc::now(),
            started_at: Some(Utc::now()),
            completed_at: None,
        };
        let val = serde_json::to_value(&item).unwrap();
        assert!(val.get("id").is_some());
        assert_eq!(val["runtime_kind"], "claude_code");
        assert_eq!(val["status"], "running");
        assert!(val.get("tokens_used").is_some());
        assert!(val.get("token_budget").is_some());
        assert!(val.get("created_at").is_some());
        assert!(val["started_at"].is_string());
        assert!(val["completed_at"].is_null());
        // Security: no api_key_id or system_prompt
        assert!(val.get("api_key_id").is_none());
        assert!(val.get("system_prompt").is_none());
    }

    #[test]
    fn session_detail_serializes_extra_fields() {
        let detail = SessionDetail {
            id: Uuid::new_v4(),
            runtime_kind: "opencode".to_owned(),
            status: "failed".to_owned(),
            input_prompt_preview: String::new(),
            workspace_path: String::new(),
            tokens_used: 0,
            token_budget: 100_000,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            pid: Some(1234),
            exit_code: Some(1),
            failed_at: Some(Utc::now()),
            failure_reason: Some("OOM".to_owned()),
        };
        let val = serde_json::to_value(&detail).unwrap();
        assert_eq!(val["pid"], 1234);
        assert_eq!(val["exit_code"], 1);
        assert!(val["failed_at"].is_string());
        assert_eq!(val["failure_reason"], "OOM");
        // Security: no api_key_id or system_prompt
        assert!(val.get("api_key_id").is_none());
        assert!(val.get("system_prompt").is_none());
    }

    #[test]
    fn list_sessions_response_wraps_sessions_array() {
        let resp = ListSessionsResponse { sessions: vec![] };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val["sessions"].is_array());
    }
}
