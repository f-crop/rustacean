use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use utoipa::ToSchema;

use crate::{error::AppError, state::AppState};

#[derive(Serialize, ToSchema)]
pub struct GithubAppHealthResponse {
    pub app_id: i64,
    pub slug: String,
    pub owner: String,
}

/// Confirms that the GitHub App private key is valid and matches what GitHub
/// knows. Uses a 60-second server-side cache to be safe for liveness probes.
///
/// Returns 503 when `RB_GH_APP_ID` / `RB_GH_APP_PRIVATE_KEY` are not set.
#[utoipa::path(
    get,
    path = "/v1/health/github-app",
    responses(
        (status = 200, description = "GitHub App identity confirmed", body = GithubAppHealthResponse),
        (status = 503, description = "GitHub App not configured"),
    ),
    tag = "health"
)]
pub async fn github_app_health(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let Some(gh) = state.gh_loader.current() else {
        return Ok((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "github_app_not_configured",
                "message": "no active control.github_app_config row and \
                            RB_GH_APP_ID / RB_GH_APP_PRIVATE_KEY are not set"
            })),
        )
            .into_response());
    };

    let identity = gh
        .check_identity()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("GitHub App health check failed: {e}")))?;

    Ok(Json(GithubAppHealthResponse {
        app_id: identity.id,
        slug: identity.slug,
        owner: identity.owner.login,
    })
    .into_response())
}
