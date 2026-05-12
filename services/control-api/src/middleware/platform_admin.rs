//! Platform-admin authorization guard.
//!
//! `is_platform_admin` is a deployment-wide flag on `control.users`, distinct
//! from the per-tenant `tenant_members.role`. Use [`require_platform_admin`]
//! to gate routes that register or replace deployment-level resources
//! (e.g. the GitHub App).
//!
//! Behaviour mirrors [`super::auth::require_verified_session`]: returns
//! `AppError::Unauthorized` for anonymous / API-key / expired-session
//! callers, `AppError::EmailNotVerified` for unverified sessions, and
//! `AppError::InsufficientRole` for verified sessions whose user row has
//! `is_platform_admin = false`.

use sqlx::PgPool;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, SessionInfo, require_verified_session},
};

/// Resolve the verified session and require `users.is_platform_admin = true`.
///
/// # Errors
///
/// Propagates the auth errors documented on
/// [`super::auth::require_verified_session`] and adds:
/// - [`AppError::InsufficientRole`] when the verified user is not a platform
///   admin.
/// - [`AppError::Database`] when the lookup fails.
// First consumer ships in Phase 3 (admin endpoints); keep it `pub` so the
// follow-up PR only adds a call-site rather than re-exporting.
#[allow(dead_code)]
pub async fn require_platform_admin(
    pool: &PgPool,
    auth: AuthContext,
) -> Result<SessionInfo, AppError> {
    let session = require_verified_session(auth)?;

    let row: Option<(bool,)> = sqlx::query_as(
        "SELECT is_platform_admin FROM control.users WHERE id = $1 AND status = 'active'",
    )
    .bind(session.user_id)
    .fetch_optional(pool)
    .await?;

    match row {
        Some((true,)) => Ok(session),
        _ => Err(AppError::InsufficientRole),
    }
}

#[cfg(test)]
mod tests {
    // The DB-touching path is covered by the integration tests that land with
    // the Phase 3 admin endpoints (they exercise the 401/403 branches end to
    // end). Pure unit coverage here would require mocking the pool, which the
    // codebase intentionally avoids — see existing patterns in
    // services/control-api/src/middleware/auth_tests.rs.
}
