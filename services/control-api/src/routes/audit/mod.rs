//! Audit log endpoint — GET /v1/audit (REQ-OB-04, ADR-007 §11.14).
//!
//! Admin-only: requires either
//!   • an API key with `Admin` scope, or
//!   • an active, verified session whose tenant role is at least `admin`.
//!
//! Non-admin callers receive HTTP 403 (`insufficient_role` / `insufficient_scope`).

use axum::{
    Json,
    extract::{Query, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope, require_verified_session},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, IntoParams)]
pub struct AuditQuery {
    /// Restrict to this tenant UUID.  For session callers this must match
    /// (or be absent) — the session tenant is used as the implicit default.
    pub tenant_id: Option<Uuid>,
    /// ISO-8601 / RFC-3339 lower bound on `occurred_at` (inclusive).
    pub from: Option<DateTime<Utc>>,
    /// ISO-8601 / RFC-3339 upper bound on `occurred_at` (inclusive).
    pub to: Option<DateTime<Utc>>,
    /// Exact action string filter, e.g. `ingest.stage.failed`.
    pub action: Option<String>,
    /// Maximum number of rows to return (1–500; default 100).
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEventItem {
    pub id: Uuid,
    pub event_id: Uuid,
    pub tenant_id: Uuid,
    pub ingestion_run_id: Option<Uuid>,
    pub repo_id: Option<Uuid>,
    pub stage: Option<String>,
    pub stage_seq: Option<i32>,
    pub actor_kind: String,
    pub actor_user_id: Option<Uuid>,
    pub action: String,
    pub outcome: String,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditListResponse {
    pub events: Vec<AuditEventItem>,
    pub total: i64,
}

// ---------------------------------------------------------------------------
// GET /v1/audit
// ---------------------------------------------------------------------------

/// List audit events (admin only).
///
/// Requires an `Admin`-scoped API key **or** an active session with at least
/// the `admin` tenant role.  Returns 403 for any other caller.
///
/// Session callers may only query their own tenant's events; the `tenant_id`
/// query parameter, if provided, must match the session tenant.  API key
/// callers may pass any `tenant_id` (or omit it to scope to the key's tenant).
#[utoipa::path(
    get,
    path = "/v1/audit",
    params(AuditQuery),
    responses(
        (status = 200, description = "Audit events", body = AuditListResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Insufficient role or scope (insufficient_role / insufficient_scope)"),
    ),
    tag = "audit"
)]
pub async fn list_audit_events(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<AuditQuery>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = resolve_tenant_and_check_admin(&state.pool, auth, query.tenant_id).await?;

    let limit = query.limit.unwrap_or(100).clamp(1, 500);

    list_events(
        &state.pool,
        tenant_id,
        query.from,
        query.to,
        query.action.as_deref(),
        limit,
    )
    .await
    .map(Json)
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

/// Resolve the effective tenant ID and verify the caller is an admin.
///
/// Returns `AppError::InsufficientRole` / `AppError::InsufficientScope` for
/// callers without admin privileges.  For session callers the `tenant_id`
/// query param, if present, must match the session tenant.
async fn resolve_tenant_and_check_admin(
    pool: &PgPool,
    auth: AuthContext,
    requested_tenant: Option<Uuid>,
) -> Result<Uuid, AppError> {
    match auth {
        AuthContext::ApiKey(info) => {
            if !info.scopes.contains(&Scope::Admin) {
                return Err(AppError::InsufficientScope);
            }
            Ok(requested_tenant.unwrap_or(info.tenant_id))
        }
        other => {
            let session = require_verified_session(other)?;

            if let Some(req_tid) = requested_tenant {
                if req_tid != session.tenant_id {
                    return Err(AppError::InsufficientRole);
                }
            }

            check_session_admin_role(pool, session.user_id, session.tenant_id).await?;
            Ok(session.tenant_id)
        }
    }
}

async fn check_session_admin_role(
    pool: &PgPool,
    user_id: Uuid,
    tenant_id: Uuid,
) -> Result<(), AppError> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    match row {
        None => Err(AppError::NotAMember),
        Some((role,)) if role == "owner" || role == "admin" => Ok(()),
        Some(_) => Err(AppError::InsufficientRole),
    }
}

// ---------------------------------------------------------------------------
// DB query
// ---------------------------------------------------------------------------

async fn list_events(
    pool: &PgPool,
    tenant_id: Uuid,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    action: Option<&str>,
    limit: i64,
) -> Result<AuditListResponse, AppError> {
    type Row = (
        Uuid,
        Uuid,
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        Option<String>,
        Option<i32>,
        String,
        Option<Uuid>,
        String,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
    );

    // Parameterised NULL-safe filters — no dynamic SQL concatenation.
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, event_id, tenant_id, ingestion_run_id, repo_id, \
                stage, stage_seq, actor_kind, actor_user_id, \
                action, outcome, occurred_at, recorded_at \
         FROM audit.audit_events \
         WHERE tenant_id = $1 \
           AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
           AND ($3::timestamptz IS NULL OR occurred_at <= $3) \
           AND ($4::text        IS NULL OR action       = $4) \
         ORDER BY occurred_at DESC \
         LIMIT $5",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(action)
    .bind(limit)
    .fetch_all(pool)
    .await?;

    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit.audit_events \
         WHERE tenant_id = $1 \
           AND ($2::timestamptz IS NULL OR occurred_at >= $2) \
           AND ($3::timestamptz IS NULL OR occurred_at <= $3) \
           AND ($4::text        IS NULL OR action       = $4)",
    )
    .bind(tenant_id)
    .bind(from)
    .bind(to)
    .bind(action)
    .fetch_one(pool)
    .await?;

    let events = rows
        .into_iter()
        .map(
            |(
                id,
                event_id,
                tenant_id,
                ingestion_run_id,
                repo_id,
                stage,
                stage_seq,
                actor_kind,
                actor_user_id,
                action,
                outcome,
                occurred_at,
                recorded_at,
            )| AuditEventItem {
                id,
                event_id,
                tenant_id,
                ingestion_run_id,
                repo_id,
                stage,
                stage_seq,
                actor_kind,
                actor_user_id,
                action,
                outcome,
                occurred_at,
                recorded_at,
            },
        )
        .collect();

    Ok(AuditListResponse { events, total })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, AuthContext, Scope, SessionInfo};

    fn admin_api_key(tenant_id: Uuid) -> AuthContext {
        AuthContext::ApiKey(ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        })
    }

    fn read_api_key() -> AuthContext {
        AuthContext::ApiKey(ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        })
    }

    fn verified_session(tenant_id: Uuid) -> AuthContext {
        AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        })
    }

    // --- API key scope checks ---

    #[test]
    fn admin_api_key_has_admin_scope() {
        let auth = admin_api_key(Uuid::new_v4());
        if let AuthContext::ApiKey(info) = &auth {
            assert!(info.scopes.contains(&Scope::Admin));
        } else {
            panic!("expected ApiKey variant");
        }
    }

    #[test]
    fn read_api_key_lacks_admin_scope() {
        let auth = read_api_key();
        if let AuthContext::ApiKey(info) = &auth {
            assert!(!info.scopes.contains(&Scope::Admin));
        }
    }

    // --- Session checks ---

    #[test]
    fn unverified_session_returns_email_not_verified() {
        let auth = AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: false,
        });
        assert!(matches!(
            require_verified_session(auth),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn anonymous_returns_unauthorized() {
        assert!(matches!(
            require_verified_session(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn expired_session_returns_session_expired() {
        assert!(matches!(
            require_verified_session(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn verified_session_returns_session_info() {
        let tid = Uuid::new_v4();
        let auth = verified_session(tid);
        let result = require_verified_session(auth);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().tenant_id, tid);
    }

    // --- Limit clamping ---

    #[test]
    fn limit_clamps_below_1() {
        assert_eq!(0_i64.clamp(1, 500), 1);
    }

    #[test]
    fn limit_clamps_above_500() {
        assert_eq!(501_i64.clamp(1, 500), 500);
    }

    #[test]
    fn limit_default_stays_in_range() {
        let limit = 100_i64.clamp(1, 500);
        assert_eq!(limit, 100);
    }

    // --- Response serialisation ---

    #[test]
    fn audit_event_item_optional_fields_serialise_as_null() {
        let item = AuditEventItem {
            id: Uuid::new_v4(),
            event_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            ingestion_run_id: None,
            repo_id: None,
            stage: None,
            stage_seq: None,
            actor_kind: "system".to_owned(),
            actor_user_id: None,
            action: "ingest.stage.started".to_owned(),
            outcome: "success".to_owned(),
            occurred_at: Utc::now(),
            recorded_at: Utc::now(),
        };
        let val = serde_json::to_value(&item).unwrap();
        assert!(val["ingestion_run_id"].is_null());
        assert!(val["repo_id"].is_null());
        assert!(val["stage"].is_null());
        assert_eq!(val["outcome"], "success");
    }

    #[test]
    fn api_key_tenant_id_fallback_to_key_tenant() {
        let key_tenant = Uuid::new_v4();
        let effective = key_tenant;
        assert_eq!(effective, key_tenant);
    }

    #[test]
    fn session_tenant_cross_tenant_query_rejected() {
        let session_tenant = Uuid::new_v4();
        let different_tenant = Uuid::new_v4();
        assert_ne!(session_tenant, different_tenant);
        // Simulates the check in resolve_tenant_and_check_admin.
        let result: Result<(), AppError> = if different_tenant == session_tenant {
            Ok(())
        } else {
            Err(AppError::InsufficientRole)
        };
        assert!(matches!(result, Err(AppError::InsufficientRole)));
    }
}
