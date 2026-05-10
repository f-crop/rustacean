use axum::{extract::State, http::StatusCode, response::IntoResponse};

use crate::{error::AppError, middleware::auth::AuthContext, state::AppState};

// ---------------------------------------------------------------------------
// POST /v1/auth/logout
// ---------------------------------------------------------------------------

/// Revoke the caller's current session and clear the `rb_session` cookie.
///
/// Sets `revoked_at = now()` on the session row, writes a `logout` row to
/// `control.auth_events` for audit, and returns `204 No Content` with a
/// `Set-Cookie` header that overwrites `rb_session` with `Max-Age=0` so the
/// browser drops it. Requires an authenticated session — anonymous and
/// API-key callers receive `401 unauthorized`.
#[utoipa::path(
    post,
    path = "/v1/auth/logout",
    responses(
        (status = 204, description = "Session revoked and cookie cleared"),
        (status = 401, description = "No active session (unauthorized)"),
    ),
    tag = "auth"
)]
pub async fn logout(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = match auth {
        AuthContext::Session(info) => info,
        // API-key callers don't have a session to revoke; only browser-style
        // session cookies are eligible to log out. Match the convention used
        // by other session-only routes (`me`, `tenants`, `api_keys`).
        AuthContext::ExpiredSession => return Err(AppError::SessionExpired),
        AuthContext::ApiKey(_) | AuthContext::Anonymous => return Err(AppError::Unauthorized),
    };

    let mut tx = state.pool.begin().await?;

    // Idempotent: only revoke once. AuthContext::Session guarantees the row
    // was active at lookup, but a concurrent revocation could race us — guard
    // with `revoked_at IS NULL` so we don't overwrite the original timestamp.
    let revoked = sqlx::query(
        "UPDATE control.sessions SET revoked_at = now() \
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(session.session_id)
    .execute(&mut *tx)
    .await?;

    // Only emit the audit event if we actually revoked the session in this
    // call, to keep auth_events free of duplicate `logout` rows when a client
    // races two logout requests.
    if revoked.rows_affected() > 0 {
        sqlx::query(
            "INSERT INTO control.auth_events (user_id, tenant_id, event) VALUES ($1, $2, 'logout')",
        )
        .bind(session.user_id)
        .bind(session.tenant_id)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;

    Ok((
        StatusCode::NO_CONTENT,
        [(
            "Set-Cookie",
            clear_session_cookie(state.config.secure_cookies),
        )],
    ))
}

/// Build the `Set-Cookie` value that clears `rb_session`.
///
/// Attributes mirror the login cookie so browsers replace the existing cookie.
/// `Max-Age=0` plus the unix-epoch `Expires` covers both modern and legacy clients.
fn clear_session_cookie(secure: bool) -> String {
    let secure_attr = if secure { " Secure;" } else { "" };
    format!(
        "rb_session=; HttpOnly; SameSite=Lax; Path=/;{secure_attr} Max-Age=0; \
         Expires=Thu, 01 Jan 1970 00:00:00 GMT"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clear_session_cookie_targets_rb_session() {
        let cookie = clear_session_cookie(true);
        assert!(cookie.starts_with("rb_session=;"), "cookie was: {cookie}");
    }

    #[test]
    fn clear_session_cookie_expires_immediately() {
        let cookie = clear_session_cookie(true);
        assert!(cookie.contains("Max-Age=0"), "cookie was: {cookie}");
        assert!(
            cookie.contains("Expires=Thu, 01 Jan 1970 00:00:00 GMT"),
            "cookie was: {cookie}",
        );
    }

    #[test]
    fn clear_session_cookie_with_secure_true_has_secure_attr() {
        let cookie = clear_session_cookie(true);
        // Browsers only overwrite a cookie when Path/SameSite/Secure match the
        // original Set-Cookie. Mirror login attributes here.
        assert!(cookie.contains("HttpOnly"), "cookie was: {cookie}");
        assert!(cookie.contains("SameSite=Lax"), "cookie was: {cookie}");
        assert!(cookie.contains("Path=/"), "cookie was: {cookie}");
        assert!(cookie.contains("Secure"), "cookie was: {cookie}");
    }

    #[test]
    fn clear_session_cookie_with_secure_false_omits_secure_attr() {
        let cookie = clear_session_cookie(false);
        assert!(cookie.contains("HttpOnly"), "cookie was: {cookie}");
        assert!(cookie.contains("SameSite=Lax"), "cookie was: {cookie}");
        assert!(cookie.contains("Path=/"), "cookie was: {cookie}");
        assert!(
            !cookie.contains("Secure"),
            "Secure must be absent: {cookie}"
        );
    }

    #[test]
    fn unauthorized_maps_to_401() {
        let err = AppError::Unauthorized;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::UNAUTHORIZED);
    }
}
