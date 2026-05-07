//! `DELETE /v1/auth/oauth/claude` — revoke and remove stored claude_code OAuth token.

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
};

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

#[utoipa::path(
    delete,
    path = "/v1/auth/oauth/claude",
    responses(
        (status = 204, description = "Token revoked"),
        (status = 401, description = "Authentication required"),
        (status = 404, description = "No stored token found"),
    ),
    tag = "auth"
)]
pub async fn claude_oauth_delete(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    // Dynamic query — agents schema not in sqlx offline cache yet.
    let result = sqlx::query(
        "DELETE FROM agents.oauth_tokens WHERE tenant_id = $1 AND user_id = $2 AND provider = 'claude_code'",
    )
    .bind(session.tenant_id)
    .bind(session.user_id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to delete oauth_token: {e}");
        AppError::Internal(anyhow::anyhow!("DB delete failed"))
    })?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }

    tracing::info!(
        tenant_id = %session.tenant_id,
        user_id = %session.user_id,
        "claude_code OAuth token deleted"
    );

    Ok(StatusCode::NO_CONTENT)
}
