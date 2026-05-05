//! `GET /v1/ingestions/recent` — recent ingestion runs (REQ-FE-08 / ADR-008 §3.9).
//!
//! Returns the most recent ingestion runs for the caller's tenant, ordered by
//! `created_at DESC`. The `trace_id` field is populated once the run's
//! trace context is propagated via migration 007.
//! Accepts verified sessions and read-scoped API keys.

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Query params
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct RecentQuery {
    /// Maximum number of runs to return (1–100; default 50).
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct RecentRunItem {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    /// 32-hex OpenTelemetry trace ID.  `null` when not yet propagated.
    pub trace_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct RecentRunsResponse {
    pub runs: Vec<RecentRunItem>,
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

fn require_read(auth: AuthContext) -> Result<Uuid, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) if info.scopes.contains(&Scope::Read) => Ok(info.tenant_id),
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// GET /v1/ingestions/recent
// ---------------------------------------------------------------------------

/// List recent ingestion runs for the caller's tenant.
///
/// Returns up to `limit` runs ordered newest-first.  Used by the
/// Activity Dashboard (REQ-FE-07) and Trace Viewer index (REQ-FE-08).
#[utoipa::path(
    get,
    path = "/v1/ingestions/recent",
    params(RecentQuery),
    responses(
        (status = 200, description = "Recent ingestion runs", body = RecentRunsResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
    ),
    tag = "ingestions"
)]
pub async fn list_recent_runs(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<RecentQuery>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read(auth)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 100);

    type Row = (Uuid, Uuid, String, Option<DateTime<Utc>>, Option<DateTime<Utc>>, Option<String>, DateTime<Utc>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, repo_id, status, started_at, finished_at, trace_id, created_at \
         FROM control.ingestion_runs \
         WHERE tenant_id = $1 \
         ORDER BY created_at DESC \
         LIMIT $2",
    )
    .bind(tenant_id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    let runs = rows
        .into_iter()
        .map(|(id, repo_id, status, started_at, finished_at, trace_id, created_at)| {
            RecentRunItem { id, repo_id, status, started_at, finished_at, trace_id, created_at }
        })
        .collect();

    Ok(Json(RecentRunsResponse { runs }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session(tenant_id: Uuid) -> AuthContext {
        AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        })
    }

    #[test]
    fn verified_session_returns_tenant_id() {
        let tid = Uuid::new_v4();
        let result = require_read(verified_session(tid));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tid);
    }

    #[test]
    fn unverified_session_rejected() {
        let auth = AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: false,
        });
        assert!(matches!(require_read(auth), Err(AppError::EmailNotVerified)));
    }

    #[test]
    fn anonymous_rejected() {
        assert!(matches!(require_read(AuthContext::Anonymous), Err(AppError::Unauthorized)));
    }

    #[test]
    fn read_api_key_accepted() {
        let auth = AuthContext::ApiKey(ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        });
        assert!(require_read(auth).is_ok());
    }

    #[test]
    fn limit_clamped() {
        assert_eq!(0_i64.clamp(1, 100), 1);
        assert_eq!(200_i64.clamp(1, 100), 100);
    }

    #[test]
    fn recent_run_item_serialises() {
        let item = RecentRunItem {
            id: Uuid::new_v4(),
            repo_id: Uuid::new_v4(),
            status: "succeeded".to_owned(),
            started_at: None,
            finished_at: None,
            trace_id: Some("abcdef0123456789abcdef0123456789".to_owned()),
            created_at: Utc::now(),
        };
        let val = serde_json::to_value(&item).unwrap();
        assert_eq!(val["status"], "succeeded");
        assert!(val["trace_id"].is_string());
    }
}
