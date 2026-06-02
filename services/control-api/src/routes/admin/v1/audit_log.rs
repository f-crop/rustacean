//! `GET /api/admin/v1/audit-log` — query the admin audit log (ADR-012 §S1.2).

use axum::{
    Extension, Json,
    extract::{Query, State},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminIp, AdminRequestId, AdminUserAgent},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

// ---------------------------------------------------------------------------
// DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
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

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditLogRow {
    pub id: i64,
    pub created_at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub tenant_id: Option<Uuid>,
    pub target_user_id: Option<Uuid>,
    pub request_id: Uuid,
    /// Client IP address extracted from `X-Forwarded-For` or peer address (nullable).
    pub ip: Option<String>,
    /// HTTP `User-Agent` header value (nullable).
    pub user_agent: Option<String>,
    pub outcome: String,
    pub error_class: Option<String>,
    pub payload_summary: serde_json::Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditLogResp {
    pub rows: Vec<AuditLogRow>,
    pub total: usize,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// List admin audit-log rows (operator-only, ADR-012 §S1.2).
///
/// Returns up to `limit` rows (default 100, max 500) in descending
/// `created_at` order.  Each row now includes the `ip` and `user_agent`
/// fields that were captured at request time (RUSAA-1801).
#[utoipa::path(
    get,
    path = "/api/admin/v1/audit-log",
    params(AuditLogQuery),
    responses(
        (status = 200, description = "Admin audit-log rows", body = AuditLogResp),
        (status = 401, description = "Missing or invalid admin bearer token"),
    ),
    tag = "admin"
)]
pub async fn list_audit_log(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Extension(AdminIp(ip)): Extension<AdminIp>,
    Extension(AdminUserAgent(user_agent)): Extension<AdminUserAgent>,
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
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT id, created_at, actor, action, tenant_id, target_user_id, request_id, \
                ip::text, user_agent, outcome, error_class, payload_summary \
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
                ip_col,
                ua_col,
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
                    ip: ip_col,
                    user_agent: ua_col,
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
        ip.as_deref(),
        user_agent.as_deref(),
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
    use chrono::Utc;

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

    /// Verify `ip` and `user_agent` round-trip through the DTO serialization.
    #[test]
    fn audit_log_row_ip_and_user_agent_serialize() {
        let row = AuditLogRow {
            id: 42,
            created_at: Utc::now(),
            actor: "test-actor".to_owned(),
            action: "audit_log.query".to_owned(),
            tenant_id: None,
            target_user_id: None,
            request_id: Uuid::new_v4(),
            ip: Some("198.51.100.1".to_owned()),
            user_agent: Some("curl/7.88".to_owned()),
            outcome: "ok".to_owned(),
            error_class: None,
            payload_summary: serde_json::json!({}),
        };
        let val = serde_json::to_value(&row).unwrap();
        assert_eq!(val["ip"], "198.51.100.1");
        assert_eq!(val["user_agent"], "curl/7.88");
    }

    /// Nullable `ip` / `user_agent` serialize as JSON `null`.
    #[test]
    fn audit_log_row_null_ip_and_user_agent_serialize_as_null() {
        let row = AuditLogRow {
            id: 1,
            created_at: Utc::now(),
            actor: "a".to_owned(),
            action: "b".to_owned(),
            tenant_id: None,
            target_user_id: None,
            request_id: Uuid::new_v4(),
            ip: None,
            user_agent: None,
            outcome: "ok".to_owned(),
            error_class: None,
            payload_summary: serde_json::json!({}),
        };
        let val = serde_json::to_value(&row).unwrap();
        assert!(val["ip"].is_null());
        assert!(val["user_agent"].is_null());
    }
}
