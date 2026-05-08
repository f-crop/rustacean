//! GET /v1/github/install-url  — generate a single-use App install URL (REQ-GH-02)
//! GET /v1/github/callback     — validate state token and create installation row (REQ-GH-02)

use axum::{
    Json,
    extract::{Query, State},
    response::{IntoResponse, Redirect},
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use urlencoding::encode as urlencode;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

#[derive(Debug, Serialize, ToSchema)]
pub struct InstallUrlResponse {
    /// Full GitHub App install URL to open in the browser.
    pub url: String,
    /// The raw opaque state token embedded in the URL.
    pub state_token: String,
}

#[utoipa::path(
    get,
    path = "/v1/github/install-url",
    responses(
        (status = 200, description = "Install URL generated", body = InstallUrlResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified"),
        (status = 503, description = "GitHub App not configured on this instance"),
    ),
    tag = "github"
)]
pub async fn github_install_url(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;
    let gh = state.gh.as_ref().ok_or(AppError::GithubAppNotConfigured)?;

    let identity = gh.check_identity().await.map_err(|e| {
        tracing::error!(error = %e, "install-url: GitHub App identity unavailable");
        AppError::Internal(anyhow::anyhow!("GitHub App identity unavailable"))
    })?;

    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let token_hex = hex::encode(raw);
    let token_hash = rb_github::hash_token(&raw);

    sqlx::query(
        "INSERT INTO control.github_install_states \
         (token_hash, tenant_id, user_id, expires_at, created_at) \
         VALUES ($1, $2, $3, now() + interval '10 minutes', now())",
    )
    .bind(&token_hash)
    .bind(session.tenant_id)
    .bind(session.user_id)
    .execute(&state.pool)
    .await?;

    let url = format!(
        "https://github.com/apps/{}/installations/new?state={}",
        identity.slug, token_hex
    );

    tracing::info!(
        tenant_id = %session.tenant_id,
        user_id = %session.user_id,
        "github install-url: state token issued"
    );

    Ok(Json(InstallUrlResponse {
        url,
        state_token: token_hex,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CallbackParams {
    pub installation_id: i64,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub setup_action: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/github/callback",
    params(
        ("installation_id" = i64, Query, description = "GitHub numeric installation ID"),
        ("state" = Option<String>, Query, description = "Opaque state token from install-url (absent for GitHub-initiated redirects)"),
        ("setup_action" = Option<String>, Query, description = "install or update"),
    ),
    responses(
        (status = 302, description = "Redirect to frontend repos page"),
        (status = 400, description = "Invalid or expired state token"),
        (status = 503, description = "GitHub App not configured on this instance"),
    ),
    tag = "github"
)]
pub async fn github_callback(
    State(state): State<AppState>,
    Query(params): Query<CallbackParams>,
) -> Result<impl IntoResponse, AppError> {
    let gh = state.gh.as_ref().ok_or(AppError::GithubAppNotConfigured)?;

    let state_hex = match params.state {
        Some(s) if !s.is_empty() => s,
        _ => {
            tracing::info!(
                installation_id = params.installation_id,
                setup_action = ?params.setup_action,
                "github callback: no state token (GitHub-initiated redirect), sending to repos"
            );
            return Ok(Redirect::to(&format!("{}/repos", state.config.base_url)));
        }
    };

    let raw = hex::decode(&state_hex).map_err(|_| AppError::InvalidToken)?;
    let token_hash = rb_github::hash_token(&raw);
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "UPDATE control.github_install_states \
         SET used_at = now() \
         WHERE token_hash = $1 \
           AND used_at IS NULL \
           AND expires_at > now() \
         RETURNING tenant_id, user_id",
    )
    .bind(&token_hash)
    .fetch_optional(&state.pool)
    .await?;

    let (tenant_id, user_id) = row.ok_or(AppError::InvalidToken)?;

    let info = gh
        .fetch_installation(params.installation_id)
        .await
        .map_err(|e| {
            tracing::error!(
                installation_id = params.installation_id,
                error = %e,
                "callback: failed to fetch installation from GitHub"
            );
            AppError::Internal(anyhow::anyhow!("failed to fetch GitHub installation"))
        })?;

    // Guard against cross-tenant hijack: only allow the upsert when the
    // existing row (if any) belongs to the same tenant. The WHERE clause on
    // DO UPDATE makes the statement a no-op when another tenant owns the row,
    // returning None instead of the UUID.
    let installation_uuid_opt: Option<(Uuid,)> = sqlx::query_as(
        "INSERT INTO control.github_installations \
         (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (github_installation_id) \
         DO UPDATE SET \
           account_login = EXCLUDED.account_login, \
           account_type  = EXCLUDED.account_type, \
           account_id    = EXCLUDED.account_id, \
           deleted_at    = NULL, \
           suspended_at  = NULL \
         WHERE github_installations.tenant_id = EXCLUDED.tenant_id \
         RETURNING id",
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
    .bind(params.installation_id)
    .bind(&info.account.login)
    .bind(&info.account.kind)
    .bind(info.account.id)
    .fetch_optional(&state.pool)
    .await?;

    let Some((installation_uuid,)) = installation_uuid_opt else {
        let owner: Option<Uuid> = sqlx::query_scalar(
            "SELECT tenant_id FROM control.github_installations \
             WHERE github_installation_id = $1",
        )
        .bind(params.installation_id)
        .fetch_optional(&state.pool)
        .await?;
        tracing::warn!(
            requesting_tenant = %tenant_id,
            owner_tenant = ?owner,
            installation_id = params.installation_id,
            "github callback: cross-tenant installation conflict rejected"
        );
        // Redirect to the frontend with an error flag so the browser renders
        // a friendly message rather than exposing the raw JSON 409 body.
        return Ok(Redirect::to(&format!(
            "{}/repos?install=conflict",
            state.config.base_url,
        )));
    };

    tracing::info!(
        tenant_id = %tenant_id,
        user_id = %user_id,
        installation_id = params.installation_id,
        installation_uuid = %installation_uuid,
        account = %info.account.login,
        setup_action = ?params.setup_action,
        "github callback: installation upserted"
    );

    Ok(Redirect::to(&format!(
        "{}/repos?install=success&installation_uuid={}&account_login={}",
        state.config.base_url,
        installation_uuid,
        urlencode(&info.account.login),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_url_response_serializes() {
        let resp = InstallUrlResponse {
            url: "https://github.com/apps/my-app/installations/new?state=abc".to_owned(),
            state_token: "abc".to_owned(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v["url"].as_str().unwrap().contains("state=abc"));
        assert_eq!(v["state_token"], "abc");
    }

    #[test]
    fn state_token_round_trip() {
        let raw = [0xdeu8; 32];
        let token_hex = hex::encode(raw);
        let hash_at_generation = rb_github::hash_token(&raw);
        let decoded = hex::decode(&token_hex).unwrap();
        let hash_at_callback = rb_github::hash_token(&decoded);
        assert_eq!(hash_at_generation, hash_at_callback);
    }

    #[test]
    fn state_token_invalid_hex_is_rejected() {
        assert!(hex::decode("not-valid-hex!").is_err());
    }

    #[test]
    fn github_installation_conflict_is_409() {
        use crate::error::AppError;
        use axum::response::IntoResponse as _;
        let resp = AppError::GithubInstallationConflict.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::CONFLICT);
    }

    /// Validates the SQL upsert guard: same-tenant re-install succeeds,
    /// cross-tenant attempt returns None (no RETURNING row).
    ///
    /// Skipped automatically when `RB_DATABASE_URL` is not set.
    #[tokio::test]
    async fn upsert_guard_rejects_cross_tenant_owner() {
        let Ok(db_url) = std::env::var("RB_DATABASE_URL") else {
            return;
        };
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&db_url)
            .await
            .expect("connect");

        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let github_installation_id: i64 = i64::from(rand::random::<i32>().abs()) + 1_000_000;

        // Seed the two test tenants (required by FK on github_installations.tenant_id).
        sqlx::query(
            "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES \
             ($1, $2, 'QA Tenant A', $3), ($4, $5, 'QA Tenant B', $6)",
        )
        .bind(tenant_a)
        .bind(format!("qa-tenant-a-{tenant_a}"))
        .bind(format!("qa_tenant_a_{}", tenant_a.simple()))
        .bind(tenant_b)
        .bind(format!("qa-tenant-b-{tenant_b}"))
        .bind(format!("qa_tenant_b_{}", tenant_b.simple()))
        .execute(&pool)
        .await
        .expect("seed tenants");

        // Seed tenant_a owning this installation.
        sqlx::query(
            "INSERT INTO control.github_installations \
             (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
             VALUES ($1, $2, $3, 'test-login', 'Organization', 9999)",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_a)
        .bind(github_installation_id)
        .execute(&pool)
        .await
        .expect("seed installation");

        // Attempt upsert from tenant_b — should return None (conflict guard).
        let result: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO control.github_installations \
             (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
             VALUES ($1, $2, $3, 'test-login', 'Organization', 9999) \
             ON CONFLICT (github_installation_id) \
             DO UPDATE SET \
               account_login = EXCLUDED.account_login, \
               account_type  = EXCLUDED.account_type, \
               account_id    = EXCLUDED.account_id, \
               deleted_at    = NULL, \
               suspended_at  = NULL \
             WHERE github_installations.tenant_id = EXCLUDED.tenant_id \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_b)
        .bind(github_installation_id)
        .fetch_optional(&pool)
        .await
        .expect("upsert query");

        assert!(
            result.is_none(),
            "cross-tenant upsert must be blocked by WHERE guard"
        );

        // Same-tenant re-install should succeed.
        let same_tenant_result: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO control.github_installations \
             (id, tenant_id, github_installation_id, account_login, account_type, account_id) \
             VALUES ($1, $2, $3, 'updated-login', 'Organization', 9999) \
             ON CONFLICT (github_installation_id) \
             DO UPDATE SET \
               account_login = EXCLUDED.account_login, \
               account_type  = EXCLUDED.account_type, \
               account_id    = EXCLUDED.account_id, \
               deleted_at    = NULL, \
               suspended_at  = NULL \
             WHERE github_installations.tenant_id = EXCLUDED.tenant_id \
             RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_a)
        .bind(github_installation_id)
        .fetch_optional(&pool)
        .await
        .expect("same-tenant upsert query");

        assert!(
            same_tenant_result.is_some(),
            "same-tenant re-install must succeed"
        );

        // Cleanup installations then tenants (FK order).
        sqlx::query("DELETE FROM control.github_installations WHERE github_installation_id = $1")
            .bind(github_installation_id)
            .execute(&pool)
            .await
            .ok();
        sqlx::query("DELETE FROM control.tenants WHERE id IN ($1, $2)")
            .bind(tenant_a)
            .bind(tenant_b)
            .execute(&pool)
            .await
            .ok();
    }
}
