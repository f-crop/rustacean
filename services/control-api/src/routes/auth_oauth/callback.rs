//! `GET /v1/auth/oauth/claude/callback` — PKCE exchange + token storage.
//!
//! Anthropic redirects here after the user approves the OAuth consent screen.
//! This handler:
//!   1. Validates the `state` param against the `rb_pkce_state` cookie.
//!   2. Exchanges the `code` for access/refresh tokens via Anthropic's token endpoint.
//!   3. Upserts the encrypted tokens into `agents.oauth_tokens`.
//!   4. Redirects the browser to the configured frontend URL.

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
};
use serde::Deserialize;
use axum_extra::extract::CookieJar;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

const PKCE_STATE_COOKIE: &str = "rb_pkce_state";
const ANTHROPIC_TOKEN_URL: &str = "https://claude.ai/oauth/token";

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub code: String,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

/// `GET /v1/auth/oauth/claude/callback` — exchange auth code for tokens.
#[utoipa::path(
    get,
    path = "/v1/auth/oauth/claude/callback",
    params(
        ("code" = String, Query, description = "Authorization code from Anthropic"),
        ("state" = String, Query, description = "CSRF state"),
    ),
    responses(
        (status = 302, description = "Redirect to frontend after successful token exchange"),
        (status = 400, description = "State mismatch or OAuth error"),
        (status = 401, description = "Authentication required"),
    ),
    tag = "auth"
)]
pub async fn claude_oauth_callback(
    State(state): State<AppState>,
    auth: AuthContext,
    cookies: CookieJar,
    Query(query): Query<CallbackQuery>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_verified_session(auth)?;

    if let Some(err) = query.error {
        tracing::warn!("Claude OAuth error: {err}");
        return Err(AppError::InvalidToken);
    }

    // Validate state + extract code_verifier from cookie.
    let pkce_cookie = cookies
        .get(PKCE_STATE_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(AppError::InvalidToken)?;

    let (cookie_state, code_verifier) = pkce_cookie
        .split_once(':')
        .ok_or(AppError::InvalidToken)?;

    if cookie_state != query.state {
        tracing::warn!("PKCE state mismatch");
        return Err(AppError::InvalidToken);
    }

    let client_id = state
        .config
        .claude_oauth_client_id
        .as_deref()
        .ok_or(AppError::RuntimeNotConfigured)?;

    let redirect_uri = format!("{}/v1/auth/oauth/claude/callback", state.config.base_url);

    // Exchange code for tokens.
    let params = [
        ("grant_type", "authorization_code"),
        ("code", query.code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];

    let resp = state
        .http_client
        .post(ANTHROPIC_TOKEN_URL)
        .form(&params)
        .send()
        .await
        .map_err(|e| {
            tracing::error!("Anthropic token exchange failed: {e}");
            AppError::Internal(anyhow::anyhow!("internal error"))
        })?;

    if !resp.status().is_success() {
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        tracing::error!("Anthropic token endpoint returned {status}: {body}");
        return Err(AppError::InvalidToken);
    }

    let token_resp: TokenResponse = resp.json().await.map_err(|e| {
        tracing::error!("failed to parse Anthropic token response: {e}");
        AppError::Internal(anyhow::anyhow!("internal error"))
    })?;

    let expires_at = token_resp.expires_in.map(|secs| {
        chrono::Utc::now() + chrono::Duration::seconds(secs)
    });

    let scopes: Vec<String> = token_resp
        .scope
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    // Upsert into agents.oauth_tokens (ON CONFLICT tenant+user+provider → UPDATE).
    // Dynamic query — agents schema not in sqlx offline cache yet.
    sqlx::query(
        r#"
        INSERT INTO agents.oauth_tokens
            (tenant_id, user_id, provider, access_token, refresh_token, expires_at, scopes, updated_at)
        VALUES ($1, $2, 'claude_code', $3, $4, $5, $6, now())
        ON CONFLICT (tenant_id, user_id, provider)
        DO UPDATE SET
            access_token  = EXCLUDED.access_token,
            refresh_token = EXCLUDED.refresh_token,
            expires_at    = EXCLUDED.expires_at,
            scopes        = EXCLUDED.scopes,
            updated_at    = now()
        "#,
    )
    .bind(session.tenant_id)
    .bind(session.user_id)
    .bind(&token_resp.access_token)
    .bind(&token_resp.refresh_token)
    .bind(expires_at)
    .bind(&scopes)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to upsert oauth_tokens: {e}");
        AppError::Internal(anyhow::anyhow!("internal error"))
    })?;

    tracing::info!(
        tenant_id = %session.tenant_id,
        user_id = %session.user_id,
        "claude_code OAuth token stored"
    );

    // Redirect to frontend.
    let frontend_url = format!("{}/settings/integrations?oauth=claude&status=success", state.config.base_url);
    Ok(Redirect::temporary(&frontend_url))
}
