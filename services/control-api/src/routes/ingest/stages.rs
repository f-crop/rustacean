//! `GET /v1/ingestions/{ingestion_run_id}/stages` — stage timeline (REQ-FE-08 / ADR-008 §3.10).
//!
//! Returns per-stage start/finish times from `pipeline_stage_runs` keyed by
//! `ingestion_run_id`.  Cross-tenant reads are blocked: the query joins
//! `ingestion_runs` to enforce `tenant_id = session.tenant_id`.
//! Accepts verified sessions and read-scoped API keys.

use axum::{Json, extract::State, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Response types (ADR-008 §3.10)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct StageRunItem {
    pub stage: String,
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct StageTimelineResponse {
    pub ingestion_run_id: Uuid,
    pub trace_id: Option<String>,
    pub stages: Vec<StageRunItem>,
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
        AuthContext::McpJwt(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// GET /v1/ingestions/{ingestion_run_id}/stages
// ---------------------------------------------------------------------------

/// Retrieve the stage timeline for an ingestion run (trace viewer fallback).
///
/// Reads `pipeline_stage_runs` ordered by stage sequence.  The cross-tenant
/// isolation check joins `ingestion_runs` to verify `tenant_id` matches the
/// caller's session/key.  Returns 404 when the run does not exist or belongs
/// to a different tenant.
#[utoipa::path(
    get,
    path = "/v1/ingestions/{ingestion_run_id}/stages",
    params(
        ("ingestion_run_id" = Uuid, Path, description = "Ingestion run UUID")
    ),
    responses(
        (status = 200, description = "Stage timeline", body = StageTimelineResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Ingestion run not found or belongs to another tenant"),
    ),
    tag = "ingest"
)]
pub async fn get_stage_timeline(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path(run_id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    type StageRow = (
        String,
        String,
        Option<DateTime<Utc>>,
        Option<DateTime<Utc>>,
        Option<String>,
    );
    let tenant_id = require_read(auth)?;

    // Verify ownership and fetch trace_id in a single query.
    let run: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, trace_id FROM control.ingestion_runs \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(run_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    let (_, trace_id) = run.ok_or(AppError::NotFound)?;

    let rows: Vec<StageRow> = sqlx::query_as(
        "SELECT psr.stage, psr.status, psr.started_at, psr.finished_at, psr.error \
         FROM control.pipeline_stage_runs psr \
         WHERE psr.ingestion_run_id = $1 \
         ORDER BY psr.created_at ASC",
    )
    .bind(run_id)
    .fetch_all(&state.pool)
    .await?;

    let stages = rows
        .into_iter()
        .map(
            |(stage, status, started_at, finished_at, error_message)| StageRunItem {
                stage,
                status,
                started_at,
                finished_at,
                error_message,
            },
        )
        .collect();

    Ok(Json(StageTimelineResponse {
        ingestion_run_id: run_id,
        trace_id,
        stages,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session() -> AuthContext {
        AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: true,
        })
    }

    #[test]
    fn verified_session_accepted() {
        assert!(require_read(verified_session()).is_ok());
    }

    #[test]
    fn anonymous_rejected() {
        assert!(matches!(
            require_read(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn read_key_accepted() {
        let auth = AuthContext::ApiKey(ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        });
        assert!(require_read(auth).is_ok());
    }

    #[test]
    fn stage_run_item_serialises() {
        let item = StageRunItem {
            stage: "clone".to_owned(),
            status: "succeeded".to_owned(),
            started_at: Some(Utc::now()),
            finished_at: None,
            error_message: None,
        };
        let val = serde_json::to_value(&item).unwrap();
        assert_eq!(val["stage"], "clone");
        assert!(val["error_message"].is_null());
    }
}
