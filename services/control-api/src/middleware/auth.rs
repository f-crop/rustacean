use axum::{
    extract::FromRequestParts,
    http::{StatusCode, header, request::Parts},
};
use chrono::{DateTime, Utc};
use rb_auth::sha256_hex;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{error::AppError, state::AppState};

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

/// Access scope for an API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Read,
    Write,
    Admin,
    Agent,
}

impl Scope {
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "admin" => Some(Scope::Admin),
            "agent" => Some(Scope::Agent),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Identity types
// ---------------------------------------------------------------------------

#[allow(dead_code, clippy::struct_field_names)]
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    /// `true` when `users.email_verified_at IS NOT NULL`.
    pub email_verified: bool,
}

/// Identity extracted from a valid API key in the `Authorization: Bearer` header.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ApiKeyInfo {
    pub key_id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub scopes: Vec<Scope>,
}

// ---------------------------------------------------------------------------
// AuthContext
// ---------------------------------------------------------------------------

/// Identity attached to every inbound request.
///
/// - `Session` — resolved from `Cookie: rb_session=<token>`
/// - `ApiKey`  — resolved from `Authorization: Bearer rb_live_<hex>`
/// - `Anonymous` — no valid credential present
#[allow(dead_code)]
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AuthContext {
    Session(SessionInfo),
    ApiKey(ApiKeyInfo),
    /// Session cookie matched a real session row but `expires_at <= now()`.
    /// Routes that require an active session should map this to `SessionExpired`
    /// (HTTP 401 `session_expired`) so the client can prompt re-login rather
    /// than treat the user as never-authenticated.
    ExpiredSession,
    Anonymous,
}

impl FromRequestParts<AppState> for AuthContext {
    type Rejection = StatusCode;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // API key via Bearer header takes precedence over session cookie.
        if let Some(token) = extract_bearer_token(parts) {
            if token.starts_with("rb_live_") {
                if let Some(info) = lookup_api_key(&state.pool, &token).await {
                    return Ok(AuthContext::ApiKey(info));
                }
                // Token looks like an API key but failed lookup → stay Anonymous
                // (don't fall through to cookie — the caller intended key auth).
                return Ok(AuthContext::Anonymous);
            }
        }
        if let Some(token) = extract_session_cookie(parts) {
            match lookup_session(&state.pool, &token).await {
                SessionLookup::Active(info) => return Ok(AuthContext::Session(info)),
                SessionLookup::Expired => return Ok(AuthContext::ExpiredSession),
                SessionLookup::NotFound => {}
            }
        }
        Ok(AuthContext::Anonymous)
    }
}

// ---------------------------------------------------------------------------
// Helpers for request credential extraction
// ---------------------------------------------------------------------------

fn extract_bearer_token(parts: &Parts) -> Option<String> {
    let value = parts.headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    value
        .strip_prefix("Bearer ")
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty())
}

fn extract_session_cookie(parts: &Parts) -> Option<String> {
    let cookie_header = parts.headers.get(header::COOKIE)?.to_str().ok()?;
    for part in cookie_header.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("rb_session=") {
            if !val.is_empty() {
                return Some(val.to_owned());
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Database lookups
// ---------------------------------------------------------------------------

/// Result of a session-cookie lookup.
///
/// We distinguish `Expired` from `NotFound` so the extractor can surface a
/// `session_expired` (401) error instead of a generic `unauthorized`. Sessions
/// that have been revoked are treated the same as never-existed (`NotFound`).
pub(crate) enum SessionLookup {
    Active(SessionInfo),
    Expired,
    NotFound,
}

pub(crate) async fn lookup_session(pool: &PgPool, token: &str) -> SessionLookup {
    let token_hash = sha256_hex(token);
    let row: Option<(Uuid, Uuid, Uuid, bool, DateTime<Utc>)> = match sqlx::query_as(
        "SELECT s.id, s.user_id, s.tenant_id, \
                (u.email_verified_at IS NOT NULL), \
                s.expires_at \
         FROM control.sessions s \
         JOIN control.users u ON u.id = s.user_id \
         WHERE s.token_hash = $1 AND s.revoked_at IS NULL",
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row,
        Err(_) => return SessionLookup::NotFound,
    };

    let Some((session_id, user_id, tenant_id, email_verified, expires_at)) = row else {
        return SessionLookup::NotFound;
    };

    if expires_at <= Utc::now() {
        return SessionLookup::Expired;
    }

    SessionLookup::Active(SessionInfo {
        session_id,
        user_id,
        tenant_id,
        email_verified,
    })
}

async fn lookup_api_key(pool: &PgPool, token: &str) -> Option<ApiKeyInfo> {
    let key_hash = sha256_hex(token);
    let row: Option<(Uuid, Uuid, Uuid, serde_json::Value)> = sqlx::query_as(
        "SELECT id, tenant_id, created_by_user_id, scopes \
         FROM control.api_keys \
         WHERE key_hash = $1 AND revoked_at IS NULL",
    )
    .bind(&key_hash)
    .fetch_optional(pool)
    .await
    .ok()?;

    let (key_id, tenant_id, user_id, scopes_json) = row?;
    let scopes = parse_scopes(&scopes_json);

    // Fire-and-forget: update last_used_at without blocking the hot path.
    let pool = pool.clone();
    tokio::spawn(async move {
        let _ = sqlx::query("UPDATE control.api_keys SET last_used_at = now() WHERE id = $1")
            .bind(key_id)
            .execute(&pool)
            .await;
    });

    Some(ApiKeyInfo {
        key_id,
        tenant_id,
        user_id,
        scopes,
    })
}

fn parse_scopes(value: &serde_json::Value) -> Vec<Scope> {
    value
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().and_then(Scope::from_str))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// Require that the caller authenticated via API key and holds the given scope.
///
/// Returns `Unauthorized` for non-API-key callers and `InsufficientScope`
/// when the key lacks the required scope.
#[allow(dead_code)]
pub fn require_scope<'a>(
    auth: &'a AuthContext,
    required: &Scope,
) -> Result<&'a ApiKeyInfo, AppError> {
    match auth {
        AuthContext::ApiKey(info) => {
            if info.scopes.contains(required) {
                Ok(info)
            } else {
                Err(AppError::InsufficientScope)
            }
        }
        _ => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// Session-check helpers
// ---------------------------------------------------------------------------

/// Require an active session whose user has a verified email.
///
/// - `Session(info)` with `email_verified == true` → `Ok(info)`
/// - `Session(info)` with `email_verified == false` → `EmailNotVerified` (403)
/// - `ExpiredSession` → `SessionExpired` (401, `session_expired`)
/// - `ApiKey` / `Anonymous` → `Unauthorized` (401)
#[allow(dead_code)]
pub fn require_verified_session(auth: AuthContext) -> Result<SessionInfo, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

/// Require either a verified session OR any API key (read | write | admin).
///
/// Returns the caller's `tenant_id` from whichever credential is present.
/// Used by read-only query endpoints that accept both session and key auth
/// per ADR-008 §3.6.
///
/// - `Session(info)` with `email_verified == true` → `Ok(tenant_id)`
/// - `Session(info)` with `email_verified == false` → `EmailNotVerified` (403)
/// - `ExpiredSession` → `SessionExpired` (401)
/// - `ApiKey(info)` (any scope) → `Ok(tenant_id)`
/// - `Anonymous` → `Unauthorized` (401)
pub fn require_read_auth(auth: AuthContext) -> Result<Uuid, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) => Ok(info.tenant_id),
        AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

/// Caller identity authorized to start or terminate an agent session.
///
/// Returned by [`require_session_or_agent_key`]; carries the tenant and the
/// human user whose credential authorized the call so the row written into
/// `agents.agent_sessions` and `control.api_keys` is attributed correctly.
#[derive(Debug, Clone, Copy)]
pub struct AgentSessionAuth {
    pub tenant_id: Uuid,
    pub user_id: Uuid,
}

/// Require either a verified browser session OR an API key with the `agent` scope.
///
/// Per ADR-009 §7, agent sessions can be started by a logged-in human OR by an
/// `agent`-scoped API key (e.g. a parent agent spawning a child). API keys
/// that lack the `agent` scope are rejected with `403 insufficient_scope`
/// rather than `401`, so callers can distinguish a missing credential from
/// a credential that exists but is not authorized for this surface.
///
/// - `Session(info)` with `email_verified == true` → `Ok(...)`
/// - `Session(info)` with `email_verified == false` → `EmailNotVerified` (403)
/// - `ExpiredSession` → `SessionExpired` (401, `session_expired`)
/// - `ApiKey(info)` containing `Scope::Agent` → `Ok(...)`
/// - `ApiKey(info)` without `Scope::Agent` → `InsufficientScope` (403)
/// - `Anonymous` → `Unauthorized` (401)
pub fn require_session_or_agent_key(auth: AuthContext) -> Result<AgentSessionAuth, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(AgentSessionAuth {
            tenant_id: info.tenant_id,
            user_id: info.user_id,
        }),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) if info.scopes.contains(&Scope::Agent) => Ok(AgentSessionAuth {
            tenant_id: info.tenant_id,
            user_id: info.user_id,
        }),
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}
// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "auth_tests.rs"]
mod tests;
