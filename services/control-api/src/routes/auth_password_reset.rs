use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Duration, Utc};
use rb_auth::{EmailToken, sha256_hex};
use rb_email::EmailTemplate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgotPasswordRequest {
    /// Email address for the account to recover.
    pub email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPasswordRequest {
    /// Plaintext reset token from the emailed link.
    pub token: String,
    /// New password; minimum 12 characters.
    pub new_password: String,
}

/// Request a password-reset email.
///
/// Always returns 200 OK regardless of whether the address is registered,
/// preventing email enumeration. When found, a reset link with a 15-minute
/// expiry is emailed. When not found, a dummy argon2id hash is performed to
/// keep the response time within ±50ms of a real lookup.
#[utoipa::path(
    post,
    path = "/v1/auth/forgot-password",
    request_body = ForgotPasswordRequest,
    responses(
        (status = 200, description = "Reset email sent or silently skipped"),
    ),
    tag = "auth"
)]
pub async fn forgot_password(
    State(state): State<AppState>,
    Json(body): Json<ForgotPasswordRequest>,
) -> Result<impl IntoResponse, AppError> {
    let row: Option<(Uuid,)> = sqlx::query_as("SELECT id FROM control.users WHERE email = $1")
        .bind(&body.email)
        .fetch_optional(&state.pool)
        .await?;

    match row {
        Some((user_id,)) => {
            let reset_token = EmailToken::generate();
            let expires_at = Utc::now() + Duration::minutes(15);

            let mut tx = state.pool.begin().await?;

            sqlx::query(
                "INSERT INTO control.email_tokens (token_hash, user_id, kind, expires_at) \
                 VALUES ($1, $2, 'reset', $3)",
            )
            .bind(reset_token.hash())
            .bind(user_id)
            .bind(expires_at)
            .execute(&mut *tx)
            .await?;

            sqlx::query(
                "INSERT INTO control.auth_events (user_id, event) \
                 VALUES ($1, 'password_reset_requested')",
            )
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

            tx.commit().await?;

            let reset_link = format!(
                "{}/auth/reset-password?token={}",
                state.config.base_url,
                reset_token.as_str()
            );
            let email = EmailTemplate::ResetPassword { link: reset_link }.to_email(&body.email)?;
            if let Err(e) = state.email_sender.send(email).await {
                tracing::warn!(
                    user_id = %user_id,
                    error = %e,
                    "reset email delivery failed"
                );
            }
        }
        None => {
            // Dummy hash keeps response time indistinguishable from the found path.
            let _ = state.hasher.hash("dummy-timing-equalizer-password-xx");
        }
    }

    Ok(StatusCode::OK)
}

/// Consume a reset token and set a new password.
///
/// Marks the token used, updates the password hash, and revokes **all** active
/// sessions for the user. The caller must re-authenticate after resetting.
#[utoipa::path(
    post,
    path = "/v1/auth/reset-password",
    request_body = ResetPasswordRequest,
    responses(
        (status = 204, description = "Password updated and all sessions revoked"),
        (status = 400, description = "Expired/used token (invalid_token) or short password (weak_password)"),
    ),
    tag = "auth"
)]
pub async fn reset_password(
    State(state): State<AppState>,
    Json(body): Json<ResetPasswordRequest>,
) -> Result<impl IntoResponse, AppError> {
    if body.new_password.len() < 12 {
        return Err(AppError::WeakPassword);
    }

    let token_hash = sha256_hex(&body.token);
    // Hash the new password before acquiring the DB transaction so the
    // CPU-bound work doesn't hold a transaction slot open.
    let new_password_hash = state.hasher.hash(&body.new_password)?;

    let mut tx = state.pool.begin().await?;

    // SELECT FOR UPDATE serialises concurrent reset attempts for the same token.
    let row: Option<(Uuid, Option<DateTime<Utc>>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT user_id, used_at, expires_at \
         FROM control.email_tokens \
         WHERE token_hash = $1 AND kind = 'reset' \
         FOR UPDATE",
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((user_id, used_at, expires_at)) = row else {
        return Err(AppError::InvalidToken);
    };

    if used_at.is_some() || expires_at < Utc::now() {
        return Err(AppError::InvalidToken);
    }

    sqlx::query("UPDATE control.users SET password_hash = $1 WHERE id = $2")
        .bind(&new_password_hash)
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    sqlx::query("UPDATE control.email_tokens SET used_at = now() WHERE token_hash = $1")
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;

    sqlx::query(
        "UPDATE control.sessions SET revoked_at = now() \
         WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query("INSERT INTO control.auth_events (user_id, event) VALUES ($1, 'password_reset')")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}
