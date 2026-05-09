use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use rb_auth::EmailToken;
use rb_email::EmailTemplate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::role::{TenantRole, require_role, require_session, urlencoding_simple};
use crate::{
    error::AppError,
    middleware::auth::AuthContext,
    state::AppState,
};

#[derive(Debug, Deserialize, ToSchema)]
pub struct InviteMemberRequest {
    /// Email address of the user to invite or add.
    pub email: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct InviteMemberResponse {
    /// `true` when the invite email was sent (user did not have an existing account).
    pub invited: bool,
    /// Present when an existing user was added directly.
    pub user_id: Option<Uuid>,
    pub email: String,
    pub role: String,
}

/// Invite a user to this tenant by email.
///
/// If the user already has an account they are added immediately with the
/// `member` role. If they do not yet have an account, an invite email is sent
/// with a signup link.
/// Requires: session with admin or owner role in the target tenant.
#[utoipa::path(
    post,
    path = "/v1/tenants/{id}/members",
    params(("id" = Uuid, Path, description = "Tenant ID")),
    request_body = InviteMemberRequest,
    responses(
        (status = 201, description = "Member added", body = InviteMemberResponse),
        (status = 202, description = "Invite email sent", body = InviteMemberResponse),
        (status = 401, description = "Not authenticated (unauthorized)"),
        (status = 403, description = "Not a member or insufficient role (not_a_member / insufficient_role)"),
        (status = 409, description = "Already a member (already_member)"),
    ),
    tag = "tenants"
)]
pub async fn invite_member(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<InviteMemberRequest>,
) -> Result<Response, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Admin).await?;

    let tenant_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM control.tenants WHERE id = $1 AND status = 'active'")
            .bind(tenant_id)
            .fetch_optional(&state.pool)
            .await?;
    let tenant_name = tenant_name.ok_or(AppError::NotFound)?;

    let existing: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM control.users WHERE email = $1")
        .bind(&body.email)
        .fetch_optional(&state.pool)
        .await?;

    if let Some((invitee_id,)) = existing {
        return add_existing_user_to_tenant(&state, tenant_id, invitee_id, &session, body.email)
            .await;
    }
    send_tenant_invite(&state, tenant_id, &tenant_name, &session, body.email).await
}

async fn add_existing_user_to_tenant(
    state: &AppState,
    tenant_id: Uuid,
    invitee_id: Uuid,
    session: &crate::middleware::auth::SessionInfo,
    email: String,
) -> Result<Response, AppError> {
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2)",
    )
    .bind(tenant_id)
    .bind(invitee_id)
    .fetch_one(&state.pool)
    .await?;
    if already {
        return Err(AppError::AlreadyMember);
    }

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role, invited_by, invited_at) \
         VALUES ($1, $2, 'member', $3, now())",
    )
    .bind(tenant_id)
    .bind(invitee_id)
    .bind(session.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'member_added', $3)",
    )
    .bind(invitee_id)
    .bind(tenant_id)
    .bind(serde_json::json!({
        "invited_by": session.user_id,
        "session_id": session.session_id,
    }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(InviteMemberResponse {
            invited: false,
            user_id: Some(invitee_id),
            email,
            role: "member".to_owned(),
        }),
    )
        .into_response())
}

async fn send_tenant_invite(
    state: &AppState,
    tenant_id: Uuid,
    tenant_name: &str,
    session: &crate::middleware::auth::SessionInfo,
    email: String,
) -> Result<Response, AppError> {
    let invite_token = EmailToken::generate();
    let invite_link = format!(
        "{}/auth/signup?email={}&tenant_invitation={}",
        state.config.base_url,
        urlencoding_simple(&email),
        invite_token.as_str(),
    );
    let email_msg = EmailTemplate::TenantInvite {
        link: invite_link,
        tenant_name: tenant_name.to_owned(),
    }
    .to_email(&email)?;
    if let Err(e) = state.email_sender.send(email_msg).await {
        tracing::warn!(
            tenant_id = %tenant_id,
            invitee_email = %email,
            error = %e,
            "invite email delivery failed"
        );
    }
    sqlx::query(
        "INSERT INTO control.auth_events (tenant_id, event, metadata) \
         VALUES ($1, 'member_invite_sent', $2)",
    )
    .bind(tenant_id)
    .bind(serde_json::json!({
        "invitee_email": &email,
        "invited_by": session.user_id,
        "session_id": session.session_id,
    }))
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(InviteMemberResponse {
            invited: true,
            user_id: None,
            email,
            role: "member".to_owned(),
        }),
    )
        .into_response())
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateRoleRequest {
    /// Target role: `member` or `admin`. Cannot set `owner` via this endpoint.
    pub role: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UpdateRoleResponse {
    pub user_id: Uuid,
    pub role: String,
}

/// Change a member's role within a tenant.
///
/// Requires: session with admin or owner role.
/// Cannot change the owner's role — use transfer-ownership for that.
/// Cannot set `owner` as the new role — use transfer-ownership.
#[utoipa::path(
    put,
    path = "/v1/tenants/{id}/members/{uid}/role",
    params(
        ("id" = Uuid, Path, description = "Tenant ID"),
        ("uid" = Uuid, Path, description = "User ID of the member to update"),
    ),
    request_body = UpdateRoleRequest,
    responses(
        (status = 200, description = "Role updated", body = UpdateRoleResponse),
        (status = 400, description = "Cannot demote owner (cannot_remove_owner) or invalid role"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Insufficient role (insufficient_role)"),
        (status = 404, description = "Member not found"),
    ),
    tag = "tenants"
)]
pub async fn update_member_role(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((tenant_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateRoleRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Admin).await?;

    let new_role = TenantRole::from_str(&body.role).ok_or(AppError::NotFound)?;
    if new_role == TenantRole::Owner {
        return Err(AppError::CannotRemoveOwner);
    }

    let current: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(target_user_id)
    .fetch_optional(&state.pool)
    .await?;
    let (current_role_str,) = current.ok_or(AppError::NotFound)?;
    if current_role_str == "owner" {
        return Err(AppError::CannotRemoveOwner);
    }

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE control.tenant_members SET role = $1 \
         WHERE tenant_id = $2 AND user_id = $3",
    )
    .bind(new_role.as_str())
    .bind(tenant_id)
    .bind(target_user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'member_role_changed', $3)",
    )
    .bind(target_user_id)
    .bind(tenant_id)
    .bind(serde_json::json!({
        "changed_by": session.user_id,
        "old_role": current_role_str,
        "new_role": new_role.as_str(),
    }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(UpdateRoleResponse {
        user_id: target_user_id,
        role: new_role.as_str().to_owned(),
    }))
}

/// Remove a member from a tenant.
///
/// Cannot remove the owner. The removed member's active sessions for this
/// tenant are immediately revoked.
/// Requires: session with admin or owner role.
#[utoipa::path(
    delete,
    path = "/v1/tenants/{id}/members/{uid}",
    params(
        ("id" = Uuid, Path, description = "Tenant ID"),
        ("uid" = Uuid, Path, description = "User ID of the member to remove"),
    ),
    responses(
        (status = 204, description = "Member removed"),
        (status = 400, description = "Cannot remove owner (cannot_remove_owner)"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Insufficient role (insufficient_role)"),
        (status = 404, description = "Member not found"),
    ),
    tag = "tenants"
)]
pub async fn remove_member(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((tenant_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Admin).await?;

    let current: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(target_user_id)
    .fetch_optional(&state.pool)
    .await?;
    let (role_str,) = current.ok_or(AppError::NotFound)?;
    if role_str == "owner" {
        return Err(AppError::CannotRemoveOwner);
    }

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "DELETE FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(target_user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE control.sessions \
         SET revoked_at = now() \
         WHERE user_id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(target_user_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'member_removed', $3)",
    )
    .bind(target_user_id)
    .bind(tenant_id)
    .bind(serde_json::json!({ "removed_by": session.user_id }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TransferOwnershipRequest {
    /// User ID of the existing member to promote to owner.
    pub user_id: Uuid,
}

/// Transfer the owner role to another existing member.
///
/// Atomically sets the current owner to `admin` and the target to `owner`.
/// The target must already be a member of the tenant.
/// Requires: session with owner role.
#[utoipa::path(
    post,
    path = "/v1/tenants/{id}/transfer-ownership",
    params(("id" = Uuid, Path, description = "Tenant ID")),
    request_body = TransferOwnershipRequest,
    responses(
        (status = 204, description = "Ownership transferred"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Insufficient role — must be owner (insufficient_role)"),
        (status = 404, description = "Target user is not a member"),
    ),
    tag = "tenants"
)]
pub async fn transfer_ownership(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<TransferOwnershipRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;
    require_role(&state.pool, session.user_id, tenant_id, TenantRole::Owner).await?;

    if body.user_id == session.user_id {
        return Ok(StatusCode::NO_CONTENT);
    }

    let target: Option<(String,)> = sqlx::query_as(
        "SELECT role FROM control.tenant_members \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(body.user_id)
    .fetch_optional(&state.pool)
    .await?;
    target.ok_or(AppError::NotFound)?;

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "UPDATE control.tenant_members SET role = 'admin' \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(session.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE control.tenant_members SET role = 'owner' \
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(tenant_id)
    .bind(body.user_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'ownership_transferred', $3)",
    )
    .bind(session.user_id)
    .bind(tenant_id)
    .bind(serde_json::json!({
        "from_user": session.user_id,
        "to_user": body.user_id,
    }))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
