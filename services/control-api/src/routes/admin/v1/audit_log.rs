//! `GET /api/admin/v1/audit-log` — query the admin audit log (ADR-012 §S1.2).

use axum::{
    Extension, Json,
    extract::{Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminRequestId},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    /// Filter to a specific tenant; omit for global view.
    pub tenant_id: Option<Uuid>,
    /// ISO-8601 lower bound (inclusive).
    pub from: Option<DateTime<Utc>>,
    /// ISO-8601 upper bound (exclusive).
    pub until: Option<DateTime<Utc>>,
    /// Maximum rows to return. Defaults to 100; max 500.
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AuditLogRow {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub tenant_id: Option<Uuid>,
    pub target_user_id: Option<Uuid>,
    pub request_id: Uuid,
    pub outcome: String,
    pub error_class: Option<String>,
    pub payload_summary: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct AuditLogResp {
    pub rows: Vec<AuditLogRow>,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

pub async fn list_audit_log(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Query(params): Query<AuditLogQuery>,
) -> Result<Json<AuditLogResp>, AppError> {
    let limit = params.limit.unwrap_or(100).clamp(1, 500);

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        DateTime<Utc>,
        String,
        String,
        Option<Uuid>,
        Option<Uuid>,
        Uuid,
        String,
        Option<String>,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT id, created_at, actor, action, tenant_id, target_user_id, request_id, \
                outcome, error_class, payload_summary \
         FROM auth.admin_audit_log \
         WHERE ($1::uuid IS NULL OR tenant_id = $1) \
           AND ($2::timestamptz IS NULL OR created_at >= $2) \
           AND ($3::timestamptz IS NULL OR created_at < $3) \
         ORDER BY created_at DESC \
         LIMIT $4",
    )
    .bind(params.tenant_id)
    .bind(params.from)
    .bind(params.until)
    .bind(limit)
    .fetch_all(&state.pool)
    .await?;

    let entries: Vec<AuditLogRow> = rows
        .into_iter()
        .map(
            |(
                id,
                created_at,
                actor_col,
                action,
                tenant_id,
                target_user_id,
                req_id,
                outcome,
                error_class,
                payload_summary,
            )| {
                AuditLogRow {
                    id,
                    created_at,
                    actor: actor_col,
                    action,
                    tenant_id,
                    target_user_id,
                    request_id: req_id,
                    outcome,
                    error_class,
                    payload_summary,
                }
            },
        )
        .collect();

    let total = entries.len();

    write_audit_row(
        &state.pool,
        &actor,
        "audit_log.query",
        params.tenant_id,
        None,
        request_id,
        None,
        None,
        &serde_json::json!({
            "tenant_id": params.tenant_id,
            "limit": limit,
            "results": total,
        }),
        "ok",
        None,
    )
    .await;

    Ok(Json(AuditLogResp {
        rows: entries,
        total,
    }))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_clamps_to_500() {
        let q = AuditLogQuery {
            tenant_id: None,
            from: None,
            until: None,
            limit: Some(99999),
        };
        let clamped = q.limit.unwrap_or(100).clamp(1, 500);
        assert_eq!(clamped, 500);
    }

    #[test]
    fn limit_defaults_to_100() {
        let q = AuditLogQuery {
            tenant_id: None,
            from: None,
            until: None,
            limit: None,
        };
        let effective = q.limit.unwrap_or(100).clamp(1, 500);
        assert_eq!(effective, 100);
    }
}
