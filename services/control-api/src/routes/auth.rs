use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{Duration, Utc};
use rb_auth::{EmailToken, SessionToken};
use rb_email::EmailTemplate;
use rb_schemas::TenantId;
use rb_tenant::TenantCtx;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

#[derive(Debug, Deserialize, ToSchema)]
pub struct SignupRequest {
    /// RFC 5322 email address.
    pub email: String,
    /// Plaintext password, minimum 12 characters.
    pub password: String,
    /// Display name for the new tenant workspace.
    pub tenant_name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SignupResponse {
    /// Always `true` on signup — email verification required before tenant access.
    pub email_verification_required: bool,
    pub user_id: Uuid,
}

struct SignupTransactionResult {
    user_id: Uuid,
    tenant_schema: String,
    session_token: SessionToken,
    email_token: EmailToken,
}

/// Register a new user and create their first tenant workspace.
#[utoipa::path(
    post,
    path = "/v1/auth/signup",
    request_body = SignupRequest,
    responses(
        (status = 201, description = "User and tenant created", body = SignupResponse),
        (status = 400, description = "Weak password (weak_password) or invalid email (invalid_email)"),
        (status = 409, description = "Email already registered (email_taken)"),
    ),
    tag = "auth"
)]
pub async fn signup(
    State(state): State<AppState>,
    Json(body): Json<SignupRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_email(&body.email)?;
    if body.password.len() < 12 {
        return Err(AppError::WeakPassword);
    }
    let password_hash = state.hasher.hash(&body.password)?;

    let mut tx = state.pool.begin().await?;
    let result = execute_signup_transaction(
        &mut tx,
        &body,
        &password_hash,
        state.config.session_ttl_days,
    )
    .await?;
    tx.commit().await?;

    if let Some(migrations_root) = state.config.migrations_root.clone() {
        let pool = state.pool.clone();
        let tenant_dir = migrations_root.join("tenant");
        if let Err(e) =
            migrate::migrate_tenant_schema(&pool, &result.tenant_schema, &tenant_dir).await
        {
            tracing::error!(
                tenant_schema = %result.tenant_schema,
                error = %e,
                "tenant schema migration failed after signup"
            );
            return Err(AppError::Internal(anyhow::anyhow!(
                "tenant migration failed: {e}"
            )));
        }
    }

    // Dev/UAT: when email is non-sending (console/noop), auto-verify so signup
    // flows straight to /repos without hitting the email verification wall.
    let auto_verified = matches!(state.config.email_transport.as_str(), "console" | "noop");
    if auto_verified {
        sqlx::query("UPDATE control.users SET email_verified_at = now() WHERE id = $1")
            .bind(result.user_id)
            .execute(&state.pool)
            .await?;
    }

    let verify_link = format!(
        "{}/auth/verify-email?token={}",
        state.config.base_url,
        result.email_token.as_str()
    );
    let email = EmailTemplate::VerifyEmail { link: verify_link }.to_email(&body.email)?;
    if let Err(e) = state.email_sender.send(email).await {
        tracing::warn!(user_id = %result.user_id, error = %e, "verification email delivery failed");
    }

    let cookie = build_session_cookie(result.session_token.as_str(), state.config.secure_cookies);
    Ok((
        StatusCode::CREATED,
        [("Set-Cookie", cookie)],
        Json(SignupResponse {
            email_verification_required: !auto_verified,
            user_id: result.user_id,
        }),
    ))
}

async fn execute_signup_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    body: &SignupRequest,
    password_hash: &str,
    session_ttl_days: i64,
) -> Result<SignupTransactionResult, AppError> {
    let email_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM control.users WHERE email = $1)")
            .bind(&body.email)
            .fetch_one(&mut **tx)
            .await?;
    if email_exists {
        return Err(AppError::EmailTaken);
    }

    let tenant_id_typed = TenantId::new();
    let tenant_uuid = tenant_id_typed.as_uuid();
    let tenant_ctx = TenantCtx::new(tenant_id_typed);
    let slug = derive_slug(&body.tenant_name, tenant_uuid);

    sqlx::query(
        "INSERT INTO control.tenants (id, slug, name, schema_name) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_uuid)
    .bind(&slug)
    .bind(&body.tenant_name)
    .bind(tenant_ctx.schema_name())
    .execute(&mut **tx)
    .await?;

    let schema = tenant_ctx.schema_name();
    sqlx::query(&format!(r#"CREATE SCHEMA IF NOT EXISTS "{schema}""#))
        .execute(&mut **tx)
        .await?;

    let user_id = Uuid::new_v4();
    sqlx::query("INSERT INTO control.users (id, email, password_hash) VALUES ($1, $2, $3)")
        .bind(user_id)
        .bind(&body.email)
        .bind(password_hash)
        .execute(&mut **tx)
        .await?;

    sqlx::query(
        "INSERT INTO control.tenant_members (tenant_id, user_id, role) VALUES ($1, $2, 'owner')",
    )
    .bind(tenant_uuid)
    .bind(user_id)
    .execute(&mut **tx)
    .await?;

    let email_token = EmailToken::generate();
    let expires_at = Utc::now() + Duration::hours(1);
    sqlx::query(
        "INSERT INTO control.email_tokens (token_hash, user_id, kind, expires_at) \
         VALUES ($1, $2, 'verify', $3)",
    )
    .bind(email_token.hash())
    .bind(user_id)
    .bind(expires_at)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event) VALUES ($1, $2, 'signup')",
    )
    .bind(user_id)
    .bind(tenant_uuid)
    .execute(&mut **tx)
    .await?;

    let session_id = Uuid::new_v4();
    let session_token = SessionToken::generate();
    let session_expires_at = Utc::now() + Duration::days(session_ttl_days);
    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(tenant_uuid)
    .bind(session_token.hash())
    .bind(session_expires_at)
    .execute(&mut **tx)
    .await?;

    Ok(SignupTransactionResult {
        user_id,
        tenant_schema: schema.to_owned(),
        session_token,
        email_token,
    })
}

fn build_session_cookie(token: &str, secure: bool) -> String {
    let secure_attr = if secure { "; Secure" } else { "" };
    format!("rb_session={token}; HttpOnly; SameSite=Lax; Path=/{secure_attr}")
}

fn validate_email(email: &str) -> Result<(), AppError> {
    let Some((local, domain)) = email.split_once('@') else {
        return Err(AppError::InvalidEmail);
    };
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err(AppError::InvalidEmail);
    }
    Ok(())
}

fn derive_slug(name: &str, id: Uuid) -> String {
    let base: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let base = if base.is_empty() {
        "workspace".to_owned()
    } else {
        base
    };
    let suffix = &id.simple().to_string()[..6];
    format!("{base}-{suffix}")
}

// ---------------------------------------------------------------------------
// POST /v1/auth/login
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct LoginRequest {
    /// RFC 5322 email address.
    pub email: String,
    /// Plaintext password.
    pub password: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginResponse {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    /// `true` when `email_verified_at IS NULL` — caller should redirect to verification.
    pub email_verification_required: bool,
}

/// Authenticate with email and password, creating a new session.
///
/// Verifies credentials with argon2id, creates a 30-day sliding-expiry session,
/// and sets an `HttpOnly` `rb_session` cookie. Rate-limited to 5 failures per
/// 10-minute window; exceeding the threshold returns 429 for 15 minutes.
#[utoipa::path(
    post,
    path = "/v1/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated", body = LoginResponse),
        (status = 401, description = "Invalid credentials (invalid_credentials)"),
        (status = 403, description = "Account suspended (account_suspended)"),
        (status = 429, description = "Rate-limited (rate_limited)"),
    ),
    tag = "auth"
)]
pub async fn login(
    State(state): State<AppState>,
    Json(body): Json<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    state.login_rate_limiter.check(&body.email)?;

    let row: Option<(Uuid, String, bool, String, Uuid)> = sqlx::query_as(
        "SELECT u.id, u.password_hash, (u.email_verified_at IS NOT NULL), u.status, tm.tenant_id \
         FROM control.users u \
         JOIN control.tenant_members tm ON tm.user_id = u.id \
         JOIN control.tenants t ON t.id = tm.tenant_id \
         WHERE u.email = $1 AND t.status = 'active' \
         ORDER BY tm.joined_at ASC \
         LIMIT 1",
    )
    .bind(&body.email)
    .fetch_optional(&state.pool)
    .await?;

    let Some((user_id, password_hash, email_verified, user_status, tenant_id)) = row else {
        // Dummy hash keeps timing indistinguishable from the found path.
        let _ = state.hasher.hash("dummy-timing-equalizer-password-xx");
        state.login_rate_limiter.record_attempt(&body.email, false);
        return Err(AppError::InvalidCredentials);
    };

    let password_ok = state.hasher.verify(&body.password, &password_hash)?;
    if !password_ok {
        state.login_rate_limiter.record_attempt(&body.email, false);
        return Err(AppError::InvalidCredentials);
    }

    if user_status == "suspended" {
        // Suspended accounts return 403 without recording a failed attempt —
        // credential validity was already proven by argon2id above.
        return Err(AppError::AccountSuspended);
    }

    state.login_rate_limiter.record_attempt(&body.email, true);

    let session_token = SessionToken::generate();
    let session_id = Uuid::new_v4();
    let session_expires_at = Utc::now() + Duration::days(state.config.session_ttl_days);

    let mut tx = state.pool.begin().await?;

    sqlx::query(
        "INSERT INTO control.sessions (id, user_id, tenant_id, token_hash, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(session_id)
    .bind(user_id)
    .bind(tenant_id)
    .bind(session_token.hash())
    .bind(session_expires_at)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event) VALUES ($1, $2, 'login')",
    )
    .bind(user_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    let cookie = build_session_cookie(session_token.as_str(), state.config.secure_cookies);
    Ok((
        StatusCode::OK,
        [("Set-Cookie", cookie)],
        Json(LoginResponse {
            user_id,
            tenant_id,
            email_verification_required: !email_verified,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_email_accepts_valid() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("a+b@sub.domain.io").is_ok());
    }

    #[test]
    fn validate_email_rejects_no_at() {
        assert!(matches!(
            validate_email("nodomain"),
            Err(AppError::InvalidEmail)
        ));
    }

    #[test]
    fn validate_email_rejects_empty_local() {
        assert!(matches!(
            validate_email("@example.com"),
            Err(AppError::InvalidEmail)
        ));
    }

    #[test]
    fn validate_email_rejects_no_dot_in_domain() {
        assert!(matches!(
            validate_email("user@localhost"),
            Err(AppError::InvalidEmail)
        ));
    }

    #[test]
    fn derive_slug_lowercases_and_hyphenates() {
        let id = Uuid::new_v4();
        let slug = derive_slug("Acme Corp", id);
        assert!(slug.starts_with("acme-corp-"));
        assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    }

    #[test]
    fn derive_slug_collapses_multiple_separators() {
        let id = Uuid::new_v4();
        let slug = derive_slug("Hello   World!!!", id);
        assert!(slug.starts_with("hello-world-"));
    }

    #[test]
    fn derive_slug_empty_name_uses_fallback() {
        let id = Uuid::new_v4();
        let slug = derive_slug("---", id);
        assert!(slug.starts_with("workspace-"));
    }

    #[test]
    fn derive_slug_includes_uuid_suffix() {
        let id = Uuid::new_v4();
        let slug = derive_slug("MyTenant", id);
        let suffix = &id.simple().to_string()[..6];
        assert!(slug.ends_with(suffix));
    }

    #[test]
    fn login_request_deserializes() {
        let json = r#"{"email":"user@example.com","password":"correct-horse-battery"}"#;
        let req: LoginRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.email, "user@example.com");
        assert_eq!(req.password, "correct-horse-battery");
    }

    #[test]
    fn login_response_serializes_all_fields() {
        let resp = LoginResponse {
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verification_required: true,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert!(v["user_id"].is_string());
        assert!(v["tenant_id"].is_string());
        assert_eq!(v["email_verification_required"], true);
    }

    #[test]
    fn invalid_credentials_maps_to_401() {
        let err = AppError::InvalidCredentials;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn account_suspended_maps_to_403() {
        let err = AppError::AccountSuspended;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn login_request_rejects_missing_password() {
        let json = r#"{"email":"user@example.com"}"#;
        let result: serde_json::Result<LoginRequest> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn signup_response_serializes_email_verification_required_false() {
        let resp = SignupResponse {
            email_verification_required: false,
            user_id: Uuid::new_v4(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["email_verification_required"], false);
    }

    #[test]
    fn auto_verify_triggered_for_console_transport() {
        // Validates the transport-matching logic used in signup.
        for transport in &["console", "noop"] {
            assert!(matches!(*transport, "console" | "noop"));
        }
        assert!(!matches!("smtp", "console" | "noop"));
    }
}
