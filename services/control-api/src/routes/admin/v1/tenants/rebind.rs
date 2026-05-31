use axum::{
    Extension, Json,
    extract::{Path, State},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::admin_auth::{AdminActor, AdminIp, AdminRequestId, AdminUserAgent},
    routes::admin::v1::write_audit_row,
    state::AppState,
};

#[derive(Debug, Deserialize)]
pub struct RebindGhInstallReq {
    /// Numeric GitHub App installation ID to rebind.
    pub github_installation_id: i64,
    /// When `true` the rebind proceeds even if the installation currently
    /// points at a different tenant. Requires `reason`.
    #[serde(default)]
    pub force: bool,
    /// Human-readable reason for the rebind (required when `force=true`).
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct RebindGhInstallResp {
    pub installation_id: Uuid,
    pub tenant_id: Uuid,
    pub previous_tenant_id: Option<Uuid>,
}

#[allow(clippy::too_many_lines)]
pub async fn rebind_gh_install(
    State(state): State<AppState>,
    Extension(AdminActor(actor)): Extension<AdminActor>,
    Extension(AdminRequestId(request_id)): Extension<AdminRequestId>,
    Extension(AdminIp(ip)): Extension<AdminIp>,
    Extension(AdminUserAgent(user_agent)): Extension<AdminUserAgent>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<RebindGhInstallReq>,
) -> Result<Response, AppError> {
    if body.force && body.reason.as_deref().unwrap_or("").trim().is_empty() {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            ip.as_deref(),
            user_agent.as_deref(),
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "force": body.force,
                "reason": body.reason,
            }),
            "denied",
            Some("force_requires_reason"),
        )
        .await;
        return Err(AppError::InvalidInput);
    }

    // Check target tenant exists.
    let tenant_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM control.tenants WHERE id = $1 AND deleted_at IS NULL)",
    )
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await?;
    if !tenant_exists {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            ip.as_deref(),
            user_agent.as_deref(),
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
            }),
            "denied",
            Some("tenant_not_found"),
        )
        .await;
        return Err(AppError::NotFound);
    }

    // Look up the installation row.
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT id, tenant_id \
         FROM control.github_installations \
         WHERE github_installation_id = $1 AND deleted_at IS NULL",
    )
    .bind(body.github_installation_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some((install_row_id, current_tenant_id)) = row else {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            ip.as_deref(),
            user_agent.as_deref(),
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
            }),
            "denied",
            Some("installation_not_found"),
        )
        .await;
        return Err(AppError::NotFound);
    };

    // Already bound to this tenant — idempotent no-op.
    if current_tenant_id == tenant_id {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            ip.as_deref(),
            user_agent.as_deref(),
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "note": "already_bound_to_tenant",
            }),
            "ok",
            None,
        )
        .await;
        return Ok(Json(RebindGhInstallResp {
            installation_id: install_row_id,
            tenant_id,
            previous_tenant_id: None,
        })
        .into_response());
    }

    // Different tenant — require force.
    if !body.force {
        write_audit_row(
            &state.pool,
            &actor,
            "tenant.rebind_gh",
            Some(tenant_id),
            None,
            request_id,
            ip.as_deref(),
            user_agent.as_deref(),
            &serde_json::json!({
                "github_installation_id": body.github_installation_id,
                "current_tenant_id": current_tenant_id,
            }),
            "denied",
            Some("cross_tenant_conflict"),
        )
        .await;
        return Err(AppError::GithubInstallationConflict);
    }

    sqlx::query("UPDATE control.github_installations SET tenant_id = $1 WHERE id = $2")
        .bind(tenant_id)
        .bind(install_row_id)
        .execute(&state.pool)
        .await?;

    write_audit_row(
        &state.pool,
        &actor,
        "tenant.rebind_gh",
        Some(tenant_id),
        None,
        request_id,
        ip.as_deref(),
        user_agent.as_deref(),
        &serde_json::json!({
            "github_installation_id": body.github_installation_id,
            "previous_tenant_id": current_tenant_id,
            "reason": body.reason,
        }),
        "ok",
        None,
    )
    .await;

    tracing::warn!(
        actor = %actor,
        %tenant_id,
        %current_tenant_id,
        github_installation_id = body.github_installation_id,
        reason = ?body.reason,
        "cross-tenant GitHub installation rebind executed"
    );

    Ok(Json(RebindGhInstallResp {
        installation_id: install_row_id,
        tenant_id,
        previous_tenant_id: Some(current_tenant_id),
    })
    .into_response())
}
