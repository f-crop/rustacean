//! `GET /v1/github/installations/{id}/available-repos` (REQ-GH-03).

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use rb_github::GhError;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

#[derive(Deserialize)]
pub struct QueryParams {
    #[serde(default = "default_page")]
    page: u32,
    #[serde(default = "default_per_page")]
    per_page: u32,
    #[serde(default)]
    include_archived: bool,
}

fn default_page() -> u32 {
    1
}
fn default_per_page() -> u32 {
    30
}

#[derive(Serialize, ToSchema)]
pub struct RepoItemResponse {
    pub id: i64,
    pub name: String,
    pub full_name: String,
    pub private: bool,
    pub archived: bool,
    pub default_branch: String,
    pub html_url: String,
}

#[derive(Serialize, ToSchema)]
pub struct ListReposResponse {
    pub total_count: u64,
    pub page: u32,
    pub per_page: u32,
    pub repositories: Vec<RepoItemResponse>,
}

#[utoipa::path(
    get,
    path = "/v1/github/installations/{id}/available-repos",
    params(
        ("id" = Uuid, Path, description = "Internal installation UUID (from github_installations)"),
        ("page" = Option<u32>, Query, description = "Page number (default 1)"),
        ("per_page" = Option<u32>, Query, description = "Results per page 1-100 (default 30)"),
        ("include_archived" = Option<bool>, Query, description = "Include archived repos (default false)"),
    ),
    responses(
        (status = 200, description = "Paginated list of repositories", body = ListReposResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified"),
        (status = 404, description = "Installation not found or not owned by this tenant"),
        (status = 409, description = "Installation belongs to a deactivated App (installation_for_different_app)"),
        (status = 503, description = "GitHub App not configured on this instance"),
    ),
    tag = "github"
)]
pub async fn list_available_repos(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(installation_id): Path<Uuid>,
    Query(params): Query<QueryParams>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    let per_page = params.per_page.clamp(1, 100);
    let page = params.page.max(1);

    let Some(gh) = state.gh_loader.current() else {
        return Err(AppError::GithubAppNotConfigured);
    };

    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT github_installation_id \
         FROM control.github_installations \
         WHERE id = $1 AND tenant_id = $2 \
           AND deleted_at IS NULL AND suspended_at IS NULL",
    )
    .bind(installation_id)
    .bind(session.tenant_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some((github_installation_id,)) = row else {
        return Err(AppError::NotFound);
    };

    // Mint the installation token first so a "wrong App" failure produces a
    // 409 instead of being swallowed into a generic 500.  If the active App
    // has changed since this installation was created, GitHub returns 404/401
    // on the access_tokens endpoint.
    match gh.installation_token(github_installation_id).await {
        Ok(_) => {} // token is now cached; list_installation_repos will reuse it
        Err(GhError::ApiError {
            status: 404 | 401, ..
        }) => {
            let slug: Option<(String,)> = sqlx::query_as(
                "SELECT slug FROM control.github_app_config \
                 WHERE is_active = true LIMIT 1",
            )
            .fetch_optional(&state.pool)
            .await?;
            let install_url = slug.map_or_else(
                || "https://github.com/apps".to_owned(),
                |(s,)| format!("https://github.com/apps/{s}/installations/new"),
            );
            tracing::warn!(
                github_installation_id,
                install_url = %install_url,
                "installation token mint failed: installation belongs to a deactivated GitHub App"
            );
            return Err(AppError::InstallationForDifferentApp { install_url });
        }
        Err(other) => return Err(AppError::Internal(anyhow::anyhow!("{other}"))),
    }

    let page_data = gh
        .list_installation_repos(github_installation_id, page, per_page)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("GitHub API error: {e}")))?;

    let repositories: Vec<RepoItemResponse> = page_data
        .repositories
        .into_iter()
        .filter(|r| params.include_archived || !r.archived)
        .map(|r| RepoItemResponse {
            id: r.id,
            name: r.name,
            full_name: r.full_name,
            private: r.private,
            archived: r.archived,
            default_branch: r.default_branch,
            html_url: r.html_url,
        })
        .collect();

    Ok(Json(ListReposResponse {
        total_count: page_data.total_count,
        page,
        per_page,
        repositories,
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use crate::error::AppError;

    #[test]
    fn installation_for_different_app_returns_409() {
        let err = AppError::InstallationForDifferentApp {
            install_url: "https://github.com/apps/rustacean-dev-4/installations/new".to_owned(),
        };
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn token_mint_404_maps_to_installation_for_different_app_not_internal() {
        // A 404 from the token-mint step must map to InstallationForDifferentApp
        // (409), not Internal (500).  This mirrors the same invariant tested in
        // routes/repos_tests.rs for the POST /v1/repos endpoint.
        use rb_github::GhError;
        let err = GhError::ApiError {
            status: 404,
            body: r#"{"message":"Integration not found"}"#.to_owned(),
        };
        let app_err = match err {
            GhError::ApiError {
                status: 404 | 401, ..
            } => AppError::InstallationForDifferentApp {
                install_url: "https://github.com/apps/test/installations/new".to_owned(),
            },
            other => AppError::Internal(anyhow::anyhow!("{other}")),
        };
        assert!(matches!(
            app_err,
            AppError::InstallationForDifferentApp { .. }
        ));
        assert_eq!(app_err.into_response().status(), StatusCode::CONFLICT);
    }

    #[test]
    fn token_mint_401_maps_to_installation_for_different_app() {
        use rb_github::GhError;
        let err = GhError::ApiError {
            status: 401,
            body: r#"{"message":"Not Found"}"#.to_owned(),
        };
        let app_err = match err {
            GhError::ApiError {
                status: 404 | 401, ..
            } => AppError::InstallationForDifferentApp {
                install_url: "https://github.com/apps/test/installations/new".to_owned(),
            },
            other => AppError::Internal(anyhow::anyhow!("{other}")),
        };
        assert!(matches!(
            app_err,
            AppError::InstallationForDifferentApp { .. }
        ));
    }

    #[test]
    fn token_mint_500_maps_to_internal() {
        use rb_github::GhError;
        let err = GhError::ApiError {
            status: 500,
            body: "Server Error".to_owned(),
        };
        let app_err = match err {
            GhError::ApiError {
                status: 404 | 401, ..
            } => AppError::InstallationForDifferentApp {
                install_url: "https://github.com/apps/test/installations/new".to_owned(),
            },
            other => AppError::Internal(anyhow::anyhow!("{other}")),
        };
        assert!(matches!(app_err, AppError::Internal(_)));
        assert_eq!(
            app_err.into_response().status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
