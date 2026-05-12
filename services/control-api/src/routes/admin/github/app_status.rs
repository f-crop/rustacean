//! `GET /v1/admin/github/app-status` — current GitHub App configuration.

use axum::{Json, extract::State, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::{auth::AuthContext, platform_admin::require_platform_admin},
    state::AppState,
};

/// Source identifying where the active App credentials came from.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppSource {
    Db,
    Env,
    None,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppStatusResponse {
    /// True when a `GhApp` is currently loaded into `state.gh_loader`.
    pub configured: bool,
    /// Where the active App came from.
    pub source: AppSource,
    /// Numeric GitHub App ID — present only when `configured == true`.
    pub app_id: Option<i64>,
    /// GitHub App slug — present only for DB-sourced configurations.
    pub slug: Option<String>,
    /// Install timestamp — present only for DB-sourced configurations.
    pub installed_at: Option<DateTime<Utc>>,
    /// Platform-admin user that installed the App — present only for
    /// DB-sourced configurations.
    pub installed_by: Option<Uuid>,
}

#[utoipa::path(
    get,
    path = "/v1/admin/github/app-status",
    responses(
        (status = 200, description = "Current App configuration", body = AppStatusResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Caller is not a platform admin"),
    ),
    tag = "admin"
)]
pub async fn get_app_status(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    require_platform_admin(&state.pool, auth).await?;

    let loaded = state.gh_loader.current();
    let configured = loaded.is_some();

    // Try the DB row first; if absent but the loader has an App, source must
    // be env (the legacy path doesn't touch this table).
    let db_row: Option<(i64, String, DateTime<Utc>, Uuid)> = sqlx::query_as(
        "SELECT app_id, slug, created_at, installed_by_user_id \
           FROM control.github_app_config \
          WHERE is_active = true \
          LIMIT 1",
    )
    .fetch_optional(&state.pool)
    .await?;

    let resp = if let Some((app_id, slug, created_at, installed_by)) = db_row {
        AppStatusResponse {
            configured,
            source: AppSource::Db,
            app_id: Some(app_id),
            slug: Some(slug),
            installed_at: Some(created_at),
            installed_by: Some(installed_by),
        }
    } else if let Some(app) = loaded {
        AppStatusResponse {
            configured: true,
            source: AppSource::Env,
            app_id: Some(app.app_id),
            slug: None,
            installed_at: None,
            installed_by: None,
        }
    } else {
        AppStatusResponse {
            configured: false,
            source: AppSource::None,
            app_id: None,
            slug: None,
            installed_at: None,
            installed_by: None,
        }
    };

    Ok(Json(resp))
}
