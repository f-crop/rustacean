//! `GET /v1/admin/github/app-callback` — consume the manifest state token,
//! exchange the one-time `code` from GitHub for App credentials, persist
//! them in `control.github_app_config`, and hot-swap `state.gh_loader`.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
};
use rb_github::{
    AppConfigStore, DEFAULT_GITHUB_API_BASE, EncryptionKey, NewAppConfig, Secret,
    exchange_manifest_code, try_build_gh_app,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use utoipa::IntoParams;

use crate::{
    error::AppError,
    middleware::{auth::AuthContext, platform_admin::require_platform_admin},
    state::AppState,
};

#[derive(Debug, Deserialize, IntoParams)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/admin/github/app-callback",
    params(CallbackParams),
    responses(
        (status = 302, description = "Redirect to FE admin success page"),
        (status = 400, description = "Missing/expired state or code"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Caller is not a platform admin"),
        (status = 502, description = "GitHub rejected the manifest exchange"),
        (status = 503, description = "RB_GH_APP_ENC_KEY is not configured"),
    ),
    tag = "admin"
)]
#[allow(clippy::too_many_lines)]
pub async fn get_app_callback(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(params): Query<CallbackParams>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_platform_admin(&state.pool, auth).await?;

    // Both `code` and `state` are required — GitHub always sends both on the
    // success path; missing values mean the operator hit this URL directly
    // and we cannot proceed.
    let code = params
        .code
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(AppError::InvalidInput)?;
    let state_hex = params
        .state
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(AppError::InvalidInput)?;

    let raw = hex::decode(state_hex).map_err(|_| AppError::InvalidToken)?;
    let token_hash: Vec<u8> = Sha256::digest(&raw).to_vec();

    // Atomically claim the state row. The partial unique index from
    // migration 017 + this UPDATE ... RETURNING combo prevents replay.
    let row: Option<(uuid::Uuid,)> = sqlx::query_as(
        "UPDATE control.github_manifest_states \
            SET consumed_at = now() \
          WHERE state_token_hash = $1 \
            AND consumed_at IS NULL \
            AND expires_at > now() \
          RETURNING initiated_by_user_id",
    )
    .bind(&token_hash)
    .fetch_optional(&state.pool)
    .await?;
    let (initiated_by_user_id,) = row.ok_or(AppError::InvalidToken)?;

    // Defensive: the caller resuming the callback should be the same user
    // who started the flow. Re-checking session.user_id here is cheap.
    if initiated_by_user_id != session.user_id {
        tracing::warn!(
            session_user_id = %session.user_id,
            initiated_by = %initiated_by_user_id,
            "app-callback: state token claimed by a different platform admin"
        );
        return Err(AppError::InvalidToken);
    }

    // The DB path requires RB_GH_APP_ENC_KEY — without it, we cannot persist.
    let key_b64 = state
        .config
        .gh_app_enc_key_b64
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            tracing::error!(
                "app-callback: RB_GH_APP_ENC_KEY missing — cannot persist manifest credentials"
            );
            AppError::ServiceUnavailable
        })?;
    let key = EncryptionKey::from_base64(key_b64).map_err(|e| {
        tracing::error!(error = %e, "app-callback: RB_GH_APP_ENC_KEY is invalid");
        AppError::Internal(anyhow::anyhow!("RB_GH_APP_ENC_KEY invalid"))
    })?;
    let store = AppConfigStore::new(state.pool.clone(), key);

    // Exchange the one-shot code for App credentials. Base URL is overridable
    // via RB_GH_API_BASE for integration tests against wiremock.
    let base = std::env::var("RB_GH_API_BASE").unwrap_or_else(|_| DEFAULT_GITHUB_API_BASE.into());
    let creds = exchange_manifest_code(&state.http_client, &base, code)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "app-callback: manifest exchange failed");
            // Anything from the GitHub API surfaces as 502 — we cannot
            // recover automatically; operator must restart the flow.
            AppError::Internal(anyhow::anyhow!("github manifest exchange failed: {e}"))
        })?;

    let new = NewAppConfig {
        app_id: creds.id,
        slug: creds.slug.clone(),
        client_id: creds.client_id,
        client_secret: Secret::new(creds.client_secret),
        private_key_pem: Secret::new(creds.pem),
        webhook_secret: Secret::new(creds.webhook_secret),
    };
    let inserted_id = store
        .insert_replacing(new, session.user_id)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "app-callback: insert_replacing failed");
            AppError::Internal(anyhow::anyhow!("failed to persist GitHub App credentials"))
        })?;

    // Reload the just-inserted row (gives us a fully-decrypted AppConfig
    // and avoids passing raw secrets back through try_build_gh_app).
    let active = store
        .load_active()
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "app-callback: failed to reload active app config");
            AppError::Internal(anyhow::anyhow!("failed to reload active app config"))
        })?
        .ok_or_else(|| {
            // Should never happen: we just inserted it. Treat as 500.
            tracing::error!("app-callback: active app config missing immediately after insert");
            AppError::Internal(anyhow::anyhow!("active app config missing after insert"))
        })?;

    let app = try_build_gh_app(&active).map_err(|e| {
        tracing::error!(error = %e, "app-callback: failed to build GhApp from stored row");
        AppError::Internal(anyhow::anyhow!("failed to build GhApp from stored row"))
    })?;
    let app = Arc::new(app);
    // Spawn the installation-token cache sweeper for the new App. Existing
    // sweepers attached to the previous (dropped) Arc terminate when the
    // last reference is released.
    app.start_token_sweep();
    state.gh_loader.set(Some(Arc::clone(&app)));

    tracing::info!(
        config_id = inserted_id,
        app_id = creds.id,
        slug = %creds.slug,
        installed_by = %session.user_id,
        "app-callback: GitHub App registered via manifest flow"
    );

    Ok(Redirect::to(&format!(
        "{}/admin/github?registered=true",
        state.config.base_url
    )))
}
