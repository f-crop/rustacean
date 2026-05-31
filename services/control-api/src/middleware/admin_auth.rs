//! Admin bearer-token middleware (ADR-012 §S1).
//!
//! Guards all `/api/admin/v1/*` routes. Two checks run in order:
//!
//! 1. **Token check** — `Authorization: Bearer $RB_ADMIN_TOKEN`. Wrong or
//!    missing → `401 Unauthorized` (generic, no body, never echoes the token).
//! 2. **Actor check** — `X-Admin-Actor` must be non-empty. Missing →
//!    `400 Bad Request` with an audit row written (`outcome='denied',
//!    error_class='missing_actor'`) so the attempt is visible (invariant §S1.6.2).
//!
//! On success the middleware inserts [`AdminActor`] and [`AdminRequestId`] into
//! request extensions. Handlers read them with `Extension<AdminActor>`.
//!
//! The audit row for successful and erroring requests is written by each
//! handler individually (invariant §S1.6.1), not here — except for the
//! missing-actor denial which happens before the handler runs.

use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Request extensions
// ---------------------------------------------------------------------------

/// The value of the `X-Admin-Actor` header — injected by this middleware for
/// downstream handlers to embed in audit rows.
#[derive(Clone, Debug)]
pub struct AdminActor(pub String);

/// The request-id for admin requests, resolved from the `X-Request-Id` header
/// set by the `SetRequestIdLayer` before this middleware runs.
#[derive(Clone, Debug)]
pub struct AdminRequestId(pub Uuid);

// ---------------------------------------------------------------------------
// Constant-time token comparison
// ---------------------------------------------------------------------------

/// Returns `true` iff `a` and `b` are identical without early exit.
/// Prevents timing attacks on the token comparison.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        // Length leak is acceptable — the secret length is not sensitive.
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Axum middleware that enforces admin token + actor presence.
///
/// # Returns
///
/// - `401` when the bearer token is absent, malformed, or incorrect.
/// - `400` when `X-Admin-Actor` is missing (audit row written).
/// - Passes through with `AdminActor` and `AdminRequestId` extensions on success.
pub async fn require_admin_token(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    // --- resolve request id (best-effort; fall back to a fresh UUID) ---
    let request_id: Uuid = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(Uuid::new_v4);

    // --- token check ---
    let token_ok = match state.config.admin_token.as_deref() {
        None | Some("") => false,
        Some(expected) => {
            let provided = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .unwrap_or("");
            constant_time_eq(expected.as_bytes(), provided.as_bytes())
        }
    };

    if !token_ok {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // --- actor check — extract header data before any await ---
    let actor = request
        .headers()
        .get("x-admin-actor")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    // Extract request metadata synchronously so no &Request borrow crosses await.
    let ip = request
        .headers()
        .get("x-forwarded-for")
        .or_else(|| request.headers().get("x-real-ip"))
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let path = request.uri().path().to_owned();

    let mut request = request;

    match actor {
        None => {
            // Audit the missing-actor attempt before refusing (invariant §S1.6.2).
            write_audit_denial(
                &state.pool,
                request_id,
                ip,
                user_agent,
                path,
                "missing_actor",
            )
            .await;
            StatusCode::BAD_REQUEST.into_response()
        }
        Some(a) => {
            request.extensions_mut().insert(AdminActor(a));
            request.extensions_mut().insert(AdminRequestId(request_id));
            next.run(request).await
        }
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

async fn write_audit_denial(
    pool: &PgPool,
    request_id: Uuid,
    ip: Option<String>,
    user_agent: Option<String>,
    path: String,
    error_class: &str,
) {
    let result = sqlx::query(
        "INSERT INTO auth.admin_audit_log \
         (actor, action, request_id, ip, user_agent, payload_summary, outcome, error_class) \
         VALUES ($1, $2, $3, $4::inet, $5, $6, 'denied', $7)",
    )
    .bind("<unknown>")
    .bind("auth.missing_actor")
    .bind(request_id)
    .bind(ip.as_deref())
    .bind(user_agent.as_deref())
    .bind(serde_json::json!({"path": path}))
    .bind(error_class)
    .execute(pool)
    .await;

    if let Err(e) = result {
        tracing::error!(error = %e, "failed to write admin audit denial row");
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_matches_equal() {
        assert!(constant_time_eq(b"secret123", b"secret123"));
    }

    #[test]
    fn constant_time_eq_rejects_different() {
        assert!(!constant_time_eq(b"secret123", b"different!"));
    }

    #[test]
    fn constant_time_eq_rejects_different_length() {
        assert!(!constant_time_eq(b"short", b"much-longer-value"));
    }

    #[test]
    fn constant_time_eq_empty_vs_empty() {
        assert!(constant_time_eq(b"", b""));
    }
}
