use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Duration, Utc};
use rb_auth::{EmailToken, sha256_hex};
use rb_email::EmailTemplate;
use serde::Deserialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize, ToSchema)]
pub struct VerifyEmailRequest {
    /// Plaintext verification token from the emailed link.
    pub token: String,
}

/// Verify an email address using a single-use token.
///
/// Accepts the plaintext token from the verification email, looks up its
/// SHA-256 hash in `email_tokens`, validates that it is unused and unexpired,
/// then sets `users.email_verified_at` and marks the token used. An
/// `email_verified` auth event is written atomically in the same transaction.
#[utoipa::path(
    post,
    path = "/v1/auth/verify-email",
    request_body = VerifyEmailRequest,
    responses(
        (status = 204, description = "Email successfully verified"),
        (status = 400, description = "Token expired, already used, or not found (invalid_token)"),
    ),
    tag = "auth"
)]
pub async fn verify_email(
    State(state): State<AppState>,
    Json(body): Json<VerifyEmailRequest>,
) -> Result<impl IntoResponse, AppError> {
    let token_hash = sha256_hex(&body.token);

    let mut tx = state.pool.begin().await?;

    // FOR UPDATE serialises concurrent verification attempts for the same token.
    let row: Option<(uuid::Uuid, Option<DateTime<Utc>>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT user_id, used_at, expires_at \
         FROM control.email_tokens \
         WHERE token_hash = $1 AND kind = 'verify' \
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

    sqlx::query(
        "UPDATE control.users SET email_verified_at = now() \
         WHERE id = $1 AND email_verified_at IS NULL",
    )
    .bind(user_id)
    .execute(&mut *tx)
    .await?;

    sqlx::query("UPDATE control.email_tokens SET used_at = now() WHERE token_hash = $1")
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;

    sqlx::query("INSERT INTO control.auth_events (user_id, event) VALUES ($1, 'email_verified')")
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// POST /v1/auth/resend-verification
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResendVerificationRequest {
    /// Email address of the account awaiting verification.
    pub email: String,
}

/// Resend a verification email.
///
/// Invalidates any existing unused verify tokens for the address and issues a
/// fresh one with a 1-hour expiry. Always returns 204 to avoid email
/// enumeration — callers cannot distinguish a known-unverified address from
/// an unknown or already-verified one. Rate-gated via the login rate limiter.
#[utoipa::path(
    post,
    path = "/v1/auth/resend-verification",
    request_body = ResendVerificationRequest,
    responses(
        (status = 204, description = "Verification email sent (or silently skipped)"),
        (status = 429, description = "Rate-limited (rate_limited)"),
    ),
    tag = "auth"
)]
pub async fn resend_verification(
    State(state): State<AppState>,
    Json(body): Json<ResendVerificationRequest>,
) -> Result<impl IntoResponse, AppError> {
    state.login_rate_limiter.check(&body.email)?;

    let row: Option<(Uuid, bool)> = sqlx::query_as(
        "SELECT id, (email_verified_at IS NOT NULL) \
         FROM control.users WHERE email = $1",
    )
    .bind(&body.email)
    .fetch_optional(&state.pool)
    .await?;

    // Only send if the user exists and has not yet verified their email.
    // Silently skip unknown addresses and already-verified accounts to
    // prevent email enumeration.
    if let Some((user_id, false)) = row {
        let email_token = EmailToken::generate();
        let expires_at = Utc::now() + Duration::hours(1);

        let mut tx = state.pool.begin().await?;

        // Expire outstanding unused tokens so old links stop working after resend.
        sqlx::query(
            "UPDATE control.email_tokens \
             SET expires_at = now() \
             WHERE user_id = $1 AND kind = 'verify' AND used_at IS NULL",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO control.email_tokens (token_hash, user_id, kind, expires_at) \
             VALUES ($1, $2, 'verify', $3)",
        )
        .bind(email_token.hash())
        .bind(user_id)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO control.auth_events (user_id, event) \
             VALUES ($1, 'verification_resent')",
        )
        .bind(user_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;

        let verify_link = format!(
            "{}/auth/verify-email?token={}",
            state.config.base_url,
            email_token.as_str()
        );
        let email = EmailTemplate::VerifyEmail { link: verify_link }.to_email(&body.email)?;
        if let Err(e) = state.email_sender.send(email).await {
            tracing::warn!(
                user_id = %user_id,
                error = %e,
                "resend verification email delivery failed"
            );
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_email_request_deserializes() {
        let json = r#"{"token":"abc123def456"}"#;
        let req: VerifyEmailRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.token, "abc123def456");
    }

    #[test]
    fn verify_email_request_rejects_missing_token() {
        let json = "{}";
        let result: serde_json::Result<VerifyEmailRequest> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_token_error_maps_to_400() {
        let err = AppError::InvalidToken;
        assert_eq!(err.to_string(), "invalid or expired token");
    }

    #[test]
    fn resend_verification_request_deserializes() {
        let json = r#"{"email":"user@example.com"}"#;
        let req: ResendVerificationRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.email, "user@example.com");
    }

    #[test]
    fn resend_verification_request_rejects_missing_email() {
        let json = "{}";
        let result: serde_json::Result<ResendVerificationRequest> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
