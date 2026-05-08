use axum::{Json, extract::State, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

// ---------------------------------------------------------------------------
// POST /v1/me/switch-tenant
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct SwitchTenantRequest {
    /// UUID of the tenant to switch to.
    pub tenant_id: Uuid,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CurrentTenant {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub role: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SwitchTenantResponse {
    pub current_tenant: CurrentTenant,
}

/// Switch the active tenant for the current session.
///
/// The caller must already be a member of the target tenant. The session's
/// `tenant_id` is updated in place and a `tenant_switched` auth event is
/// written. Returns the new active tenant with the caller's role.
/// Requires: verified session.
#[utoipa::path(
    post,
    path = "/v1/me/switch-tenant",
    request_body = SwitchTenantRequest,
    responses(
        (status = 200, description = "Tenant switched", body = SwitchTenantResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Email not verified (email_not_verified) or not a member (not_a_member)"),
        (status = 404, description = "Target tenant not found or inactive"),
    ),
    tag = "me"
)]
pub async fn switch_tenant(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<SwitchTenantRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let mut tx = state.pool.begin().await?;

    // FOR SHARE prevents a concurrent revocation from deleting/updating this row
    // between our membership check and the session UPDATE that follows.
    let member: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2 \
         FOR SHARE",
    )
    .bind(body.tenant_id)
    .bind(session.user_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (role,) = member.ok_or(AppError::NotAMember)?;

    // FOR SHARE prevents the tenant from being deactivated between check and commit.
    let tenant: Option<(String, String)> = sqlx::query_as(
        "SELECT name, slug::text FROM control.tenants \
         WHERE id = $1 AND status = 'active' \
         FOR SHARE",
    )
    .bind(body.tenant_id)
    .fetch_optional(&mut *tx)
    .await?;

    let (name, slug) = tenant.ok_or(AppError::NotFound)?;

    sqlx::query("UPDATE control.sessions SET tenant_id = $1 WHERE id = $2")
        .bind(body.tenant_id)
        .bind(session.session_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'tenant_switched', $3)",
    )
    .bind(session.user_id)
    .bind(body.tenant_id)
    .bind(serde_json::json!({
        "from_tenant": session.tenant_id,
        "to_tenant": body.tenant_id,
        "session_id": session.session_id,
    }))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(Json(SwitchTenantResponse {
        current_tenant: CurrentTenant {
            id: body.tenant_id,
            name,
            slug,
            role,
        },
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/me
// ---------------------------------------------------------------------------

/// Public profile of the authenticated user.
#[derive(Debug, Serialize, ToSchema)]
pub struct UserInfo {
    pub id: Uuid,
    pub email: String,
    pub status: String,
    pub email_verified: bool,
    pub created_at: DateTime<Utc>,
}

/// One tenant the caller is a member of, with their role.
#[derive(Debug, Serialize, ToSchema)]
pub struct TenantWithRole {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub role: String,
}

/// Response body of `GET /v1/me`: who the caller is, what tenant they are
/// currently scoped to, and every other tenant available to them.
#[derive(Debug, Serialize, ToSchema)]
pub struct MeResponse {
    pub user: UserInfo,
    pub current_tenant: TenantWithRole,
    pub available_tenants: Vec<TenantWithRole>,
}

/// Return the authenticated user's profile, current tenant, and available
/// tenants. As a side-effect, refreshes the session's `last_seen_at` and
/// extends `expires_at` by `session_ttl_days` so the session is sliding
/// rather than fixed-from-login. The refresh is fire-and-forget — failing
/// to extend the session does not fail the request (see REQ-AU-06).
#[utoipa::path(
    get,
    path = "/v1/me",
    responses(
        (status = 200, description = "Current user profile", body = MeResponse),
        (status = 401, description = "Not authenticated, or session expired (`session_expired`)"),
        (status = 403, description = "Email not verified (`email_not_verified`)"),
    ),
    tag = "me"
)]
pub async fn get_me(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    // Q1: user + current tenant in one shot, anchored by session_id.
    let row: (
        Uuid,
        String,
        String,
        bool,
        DateTime<Utc>,
        Uuid,
        String,
        String,
        String,
    ) = sqlx::query_as(
        "SELECT u.id, u.email::text, u.status, \
                    (u.email_verified_at IS NOT NULL), u.created_at, \
                    t.id, t.name, t.slug::text, tm.role \
             FROM control.users u \
             JOIN control.sessions s ON s.user_id = u.id \
             JOIN control.tenants t ON t.id = s.tenant_id \
             JOIN control.tenant_members tm \
                 ON tm.tenant_id = t.id AND tm.user_id = u.id \
             WHERE s.id = $1",
    )
    .bind(session.session_id)
    .fetch_one(&state.pool)
    .await?;

    let (
        user_id,
        email,
        user_status,
        email_verified,
        user_created_at,
        cur_tenant_id,
        cur_name,
        cur_slug,
        cur_role,
    ) = row;

    // Q2: every active tenant the user belongs to, ordered by membership age.
    let tenants: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT t.id, t.name, t.slug::text, tm.role \
         FROM control.tenant_members tm \
         JOIN control.tenants t ON t.id = tm.tenant_id \
         WHERE tm.user_id = $1 AND t.status = 'active' \
         ORDER BY tm.joined_at",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await?;

    let available_tenants = tenants
        .into_iter()
        .map(|(id, name, slug, role)| TenantWithRole {
            id,
            name,
            slug,
            role,
        })
        .collect();

    // Fire-and-forget sliding-window refresh. We do NOT await on this; the
    // GET succeeds even if the UPDATE fails (e.g. pool exhaustion). This
    // mirrors the existing `lookup_api_key` last_used_at pattern.
    let pool = state.pool.clone();
    let sid = session.session_id;
    // `session_ttl_days` is bounded by config (default 30, env-overridable).
    // `i32` is what `make_interval(days => ...)` expects; clamp defensively
    // so a misconfigured value cannot panic the spawned task.
    let ttl_days: i32 = i32::try_from(state.config.session_ttl_days).unwrap_or(30);
    tokio::spawn(async move {
        let _ = sqlx::query(
            "UPDATE control.sessions \
             SET last_seen_at = now(), \
                 expires_at = now() + make_interval(days => $2) \
             WHERE id = $1",
        )
        .bind(sid)
        .bind(ttl_days)
        .execute(&pool)
        .await;
    });

    Ok(Json(MeResponse {
        user: UserInfo {
            id: user_id,
            email,
            status: user_status,
            email_verified,
            created_at: user_created_at,
        },
        current_tenant: TenantWithRole {
            id: cur_tenant_id,
            name: cur_name,
            slug: cur_slug,
            role: cur_role,
        },
        available_tenants,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{AuthContext, SessionInfo};

    fn make_session(email_verified: bool) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified,
        }
    }

    #[test]
    fn switch_tenant_request_deserializes() {
        let json = r#"{"tenant_id":"550e8400-e29b-41d4-a716-446655440000"}"#;
        let req: SwitchTenantRequest = serde_json::from_str(json).unwrap();
        assert_eq!(
            req.tenant_id.to_string(),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn switch_tenant_response_serializes_all_fields() {
        let resp = SwitchTenantResponse {
            current_tenant: CurrentTenant {
                id: Uuid::new_v4(),
                name: "Test Corp".to_owned(),
                slug: "test-corp-abc123".to_owned(),
                role: "admin".to_owned(),
            },
        };
        let json = serde_json::to_value(&resp).unwrap();
        let ct = &json["current_tenant"];
        assert!(ct.get("id").is_some());
        assert_eq!(ct["name"], "Test Corp");
        assert_eq!(ct["slug"], "test-corp-abc123");
        assert_eq!(ct["role"], "admin");
    }

    #[test]
    fn anonymous_auth_returns_unauthorized() {
        assert!(matches!(
            require_verified_session(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn unverified_session_returns_email_not_verified() {
        let auth = AuthContext::Session(make_session(false));
        assert!(matches!(
            require_verified_session(auth),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn verified_session_returns_info() {
        let info = make_session(true);
        let auth = AuthContext::Session(info.clone());
        let result = require_verified_session(auth).unwrap();
        assert_eq!(result.user_id, info.user_id);
        assert_eq!(result.session_id, info.session_id);
    }

    #[test]
    fn not_a_member_error_message() {
        assert_eq!(
            AppError::NotAMember.to_string(),
            "user is not a member of this tenant"
        );
    }

    #[test]
    fn me_response_serializes_full_shape() {
        let user_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let resp = MeResponse {
            user: UserInfo {
                id: user_id,
                email: "alice@example.com".to_owned(),
                status: "active".to_owned(),
                email_verified: true,
                created_at: chrono::Utc::now(),
            },
            current_tenant: TenantWithRole {
                id: tenant_id,
                name: "Acme".to_owned(),
                slug: "acme-x1".to_owned(),
                role: "owner".to_owned(),
            },
            available_tenants: vec![TenantWithRole {
                id: tenant_id,
                name: "Acme".to_owned(),
                slug: "acme-x1".to_owned(),
                role: "owner".to_owned(),
            }],
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["user"]["email"], "alice@example.com");
        assert_eq!(v["user"]["status"], "active");
        assert_eq!(v["user"]["email_verified"], true);
        assert!(v["user"]["created_at"].is_string());
        assert_eq!(v["current_tenant"]["role"], "owner");
        assert!(v["available_tenants"].is_array());
        assert_eq!(v["available_tenants"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn user_info_serializes_uuid_as_string() {
        let info = UserInfo {
            id: Uuid::new_v4(),
            email: "u@example.com".to_owned(),
            status: "active".to_owned(),
            email_verified: false,
            created_at: chrono::Utc::now(),
        };
        let v = serde_json::to_value(&info).unwrap();
        assert!(v["id"].is_string());
        assert_eq!(v["email_verified"], false);
    }

    #[test]
    fn tenant_with_role_serializes_all_fields() {
        let t = TenantWithRole {
            id: Uuid::new_v4(),
            name: "Acme".to_owned(),
            slug: "acme-1".to_owned(),
            role: "admin".to_owned(),
        };
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["name"], "Acme");
        assert_eq!(v["slug"], "acme-1");
        assert_eq!(v["role"], "admin");
        assert!(v["id"].is_string());
    }

    #[test]
    fn require_verified_session_rejects_unverified_email() {
        use crate::middleware::auth::require_verified_session;
        let info = SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: false,
        };
        let auth = AuthContext::Session(info);
        assert!(matches!(
            require_verified_session(auth),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn require_verified_session_maps_expired_to_session_expired() {
        use crate::middleware::auth::require_verified_session;
        assert!(matches!(
            require_verified_session(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn require_verified_session_accepts_verified_session() {
        use crate::middleware::auth::require_verified_session;
        let info = make_session(true);
        let user_id = info.user_id;
        let auth = AuthContext::Session(info);
        let extracted = require_verified_session(auth).expect("verified session");
        assert_eq!(extracted.user_id, user_id);
    }

    #[test]
    fn require_verified_session_rejects_anonymous() {
        use crate::middleware::auth::require_verified_session;
        assert!(matches!(
            require_verified_session(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn email_not_verified_error_message() {
        assert_eq!(
            AppError::EmailNotVerified.to_string(),
            "email address not yet verified"
        );
    }
}
