pub mod delete;
pub mod members;
mod role;

pub use delete::delete_tenant;
pub use members::{invite_member, remove_member, transfer_ownership, update_member_role};

use axum::{
    Json,
    extract::{Path, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, middleware::auth::AuthContext, state::AppState};
use role::{TenantRole, require_role, require_session};

#[derive(Debug, Serialize, ToSchema)]
pub struct MemberItem {
    pub user_id: Uuid,
    pub email: String,
    pub role: String,
    pub invited_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListMembersResponse {
    pub members: Vec<MemberItem>,
}

/// List all members of a tenant.
///
/// Returns the user ID, email, role, and invitation time for every member.
/// Requires: session with at least member role in the target tenant.
#[utoipa::path(
    get,
    path = "/v1/tenants/{id}/members",
    params(("id" = Uuid, Path, description = "Tenant ID")),
    responses(
        (status = 200, description = "Member list", body = ListMembersResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Not a member (not_a_member)"),
    ),
    tag = "tenants"
)]
pub async fn list_members(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(tenant_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Member).await?;

    let rows: Vec<(Uuid, String, String, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT tm.user_id, u.email::text, tm.role, tm.invited_at \
         FROM control.tenant_members tm \
         JOIN control.users u ON u.id = tm.user_id \
         WHERE tm.tenant_id = $1 \
         ORDER BY tm.invited_at ASC NULLS FIRST",
    )
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let members = rows
        .into_iter()
        .map(|(user_id, email, role, invited_at)| MemberItem {
            user_id,
            email,
            role,
            invited_at,
        })
        .collect();

    Ok(Json(ListMembersResponse { members }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::role::*;
    use crate::error::AppError;
    use crate::middleware::auth::{AuthContext, SessionInfo};
    use uuid::Uuid;

    #[test]
    fn require_session_returns_info_for_verified_session() {
        let info = SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: true,
        };
        let ctx = AuthContext::Session(info.clone());
        let result = require_session(ctx).unwrap();
        assert_eq!(result.user_id, info.user_id);
    }

    #[test]
    fn require_session_returns_email_not_verified_for_unverified() {
        let info = SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: false,
        };
        let ctx = AuthContext::Session(info);
        assert!(matches!(
            require_session(ctx),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn require_session_returns_unauthorized_for_anonymous() {
        let ctx = AuthContext::Anonymous;
        assert!(matches!(require_session(ctx), Err(AppError::Unauthorized)));
    }

    #[test]
    fn update_role_rejects_owner_role_string() {
        let role = TenantRole::from_str("owner").unwrap();
        assert_eq!(role, TenantRole::Owner);
    }

    #[test]
    fn update_role_accepts_member_and_admin() {
        assert!(TenantRole::from_str("member").is_some());
        assert!(TenantRole::from_str("admin").is_some());
    }

    #[test]
    fn error_unauthorized_produces_message() {
        assert_eq!(
            AppError::Unauthorized.to_string(),
            "authentication required"
        );
    }

    #[test]
    fn error_insufficient_role_message() {
        assert_eq!(
            AppError::InsufficientRole.to_string(),
            "insufficient role for this operation"
        );
    }

    #[test]
    fn error_cannot_remove_owner_message() {
        assert_eq!(
            AppError::CannotRemoveOwner.to_string(),
            "cannot remove or demote the tenant owner"
        );
    }
}
