//! `GET /v1/auth/oauth/claude/callback` — PKCE exchange + token storage.
//!
//! Anthropic redirects here after the user approves the OAuth consent screen.
//! This handler:
//!   1. Validates the `state` param against the `rb_pkce_state` cookie.
//!   2. Exchanges the `code` for access/refresh tokens via Anthropic's token endpoint.
//!   3. Upserts the encrypted tokens into `agents.oauth_tokens`.
//!   4. Redirects the browser to the post-OAuth URI stored in the PKCE cookie, or the
//!      default integrations page when none was supplied.
//!
//! Cookie format (set by `start.rs`):
//!   `{state}:{code_verifier}`                     — no custom redirect
//!   `{state}:{code_verifier}:{b64_redirect_uri}`  — custom redirect (base64url, no pad)

use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
};
use axum_extra::extract::CookieJar;
use base64::Engine as _;
use serde::Deserialize;

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

    // Validate state + extract code_verifier (and optional post-oauth redirect) from cookie.
    let pkce_cookie = cookies
        .get(PKCE_STATE_COOKIE)
        .map(|c| c.value().to_owned())
        .ok_or(AppError::InvalidToken)?;

    // Cookie format: "{state}:{code_verifier}" or "{state}:{code_verifier}:{b64_redirect}"
    // splitn(3) keeps the third segment intact regardless of colons inside the redirect.
    let parts: Vec<&str> = pkce_cookie.splitn(3, ':').collect();
    let (cookie_state, code_verifier, post_oauth_redirect) = match parts.as_slice() {
        [s, cv, b64] => {
            let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(b64)
                .map_err(|_| AppError::InvalidToken)?;
            let uri = String::from_utf8(decoded).map_err(|_| AppError::InvalidToken)?;
            (*s, *cv, Some(uri))
        }
        [s, cv] => (*s, *cv, None),
        _ => return Err(AppError::InvalidToken),
    };

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

    let token_resp = exchange_code_for_tokens(
        &state.http_client,
        client_id,
        query.code.as_str(),
        redirect_uri.as_str(),
        code_verifier,
    )
    .await?;

    let expires_at = token_resp
        .expires_in
        .map(|secs| chrono::Utc::now() + chrono::Duration::seconds(secs));
    let scopes: Vec<String> = token_resp
        .scope
        .as_deref()
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default();

    encrypt_and_upsert_tokens(&state, session.tenant_id, session.user_id, &token_resp, expires_at, &scopes).await?;

    tracing::info!(
        tenant_id = %session.tenant_id,
        user_id = %session.user_id,
        "claude_code OAuth token stored"
    );

    let destination = post_oauth_redirect.unwrap_or_else(|| {
        format!(
            "{}/settings/integrations?oauth=claude&status=success",
            state.config.base_url
        )
    });
    Ok(Redirect::temporary(&destination))
}

async fn exchange_code_for_tokens(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    redirect_uri: &str,
    code_verifier: &str,
) -> Result<TokenResponse, AppError> {
    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id),
        ("code_verifier", code_verifier),
    ];

    let resp = http
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

    resp.json().await.map_err(|e| {
        tracing::error!("failed to parse Anthropic token response: {e}");
        AppError::Internal(anyhow::anyhow!("internal error"))
    })
}

async fn encrypt_and_upsert_tokens(
    state: &AppState,
    tenant_id: uuid::Uuid,
    user_id: uuid::Uuid,
    token_resp: &TokenResponse,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    scopes: &[String],
) -> Result<(), AppError> {
    let (stored_access, stored_refresh, key_id) = match state.token_cipher.as_deref() {
        Some(cipher) => {
            let enc_access = cipher
                .encrypt(&token_resp.access_token, user_id)
                .map_err(|e| {
                    tracing::error!("failed to encrypt OAuth access_token: {e}");
                    AppError::Internal(anyhow::anyhow!("internal error"))
                })?;
            let enc_refresh = token_resp
                .refresh_token
                .as_deref()
                .map(|rt| cipher.encrypt(rt, user_id))
                .transpose()
                .map_err(|e| {
                    tracing::error!("failed to encrypt OAuth refresh_token: {e}");
                    AppError::Internal(anyhow::anyhow!("internal error"))
                })?;
            (enc_access, enc_refresh, cipher.key_id().to_owned())
        }
        None => (
            token_resp.access_token.clone(),
            token_resp.refresh_token.clone(),
            "none".to_owned(),
        ),
    };

    sqlx::query(
        r"
        INSERT INTO agents.oauth_tokens
            (tenant_id, user_id, provider, access_token, refresh_token,
             expires_at, scopes, encryption_key_id, updated_at)
        VALUES ($1, $2, 'claude_code', $3, $4, $5, $6, $7, now())
        ON CONFLICT (tenant_id, user_id, provider)
        DO UPDATE SET
            access_token      = EXCLUDED.access_token,
            refresh_token     = EXCLUDED.refresh_token,
            expires_at        = EXCLUDED.expires_at,
            scopes            = EXCLUDED.scopes,
            encryption_key_id = EXCLUDED.encryption_key_id,
            updated_at        = now()
        ",
    )
    .bind(tenant_id)
    .bind(user_id)
    .bind(&stored_access)
    .bind(&stored_refresh)
    .bind(expires_at)
    .bind(scopes)
    .bind(&key_id)
    .execute(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("failed to upsert oauth_tokens: {e}");
        AppError::Internal(anyhow::anyhow!("internal error"))
    })?;

    Ok(())
}
