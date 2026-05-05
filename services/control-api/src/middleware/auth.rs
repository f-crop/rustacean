use axum::{
    extract::FromRequestParts,
    http::{header, request::Parts, StatusCode},
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
}

impl Scope {
    pub(crate) fn from_str(s: &str) -> Option<Self> {
        match s {
            "read" => Some(Scope::Read),
            "write" => Some(Scope::Write),
            "admin" => Some(Scope::Admin),
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
    value.strip_prefix("Bearer ").map(|t| t.trim().to_owned()).filter(|t| !t.is_empty())
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

    SessionLookup::Active(SessionInfo { session_id, user_id, tenant_id, email_verified })
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
        let _ = sqlx::query(
            "UPDATE control.api_keys SET last_used_at = now() WHERE id = $1",
        )
        .bind(key_id)
        .execute(&pool)
        .await;
    });

    Some(ApiKeyInfo { key_id, tenant_id, user_id, scopes })
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
pub fn require_scope<'a>(auth: &'a AuthContext, required: &Scope) -> Result<&'a ApiKeyInfo, AppError> {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    fn parts_with_cookie(cookie: &str) -> Parts {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        let mut req = axum::http::Request::builder().body(()).unwrap();
        *req.headers_mut() = headers;
        req.into_parts().0
    }

    fn parts_with_bearer(token: &str) -> Parts {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        let mut req = axum::http::Request::builder().body(()).unwrap();
        *req.headers_mut() = headers;
        req.into_parts().0
    }

    fn make_session(email_verified: bool) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified,
        }
    }

    #[test]
    fn extract_session_cookie_finds_rb_session() {
        let parts = parts_with_cookie("rb_session=abc123; other=val");
        assert_eq!(extract_session_cookie(&parts).as_deref(), Some("abc123"));
    }

    #[test]
    fn extract_session_cookie_works_when_first() {
        let parts = parts_with_cookie("rb_session=tok42");
        assert_eq!(extract_session_cookie(&parts).as_deref(), Some("tok42"));
    }

    #[test]
    fn extract_session_cookie_returns_none_when_absent() {
        let parts = parts_with_cookie("other=value");
        assert!(extract_session_cookie(&parts).is_none());
    }

    #[test]
    fn extract_session_cookie_ignores_empty_value() {
        let parts = parts_with_cookie("rb_session=");
        assert!(extract_session_cookie(&parts).is_none());
    }

    #[test]
    fn extract_session_cookie_handles_whitespace_around_parts() {
        let parts = parts_with_cookie("first=a;  rb_session=tok99  ;last=b");
        assert!(extract_session_cookie(&parts).is_some());
    }

    #[test]
    fn extract_bearer_token_parses_valid_header() {
        let parts = parts_with_bearer("rb_live_abc123def456");
        assert_eq!(extract_bearer_token(&parts).as_deref(), Some("rb_live_abc123def456"));
    }

    #[test]
    fn extract_bearer_token_returns_none_when_absent() {
        let parts = parts_with_cookie("rb_session=tok");
        assert!(extract_bearer_token(&parts).is_none());
    }

    #[test]
    fn extract_bearer_token_trims_whitespace() {
        let parts = parts_with_bearer("  rb_live_abc  ");
        assert_eq!(extract_bearer_token(&parts).as_deref(), Some("rb_live_abc"));
    }

    #[test]
    fn parse_scopes_extracts_known_values() {
        let json = serde_json::json!(["read", "write"]);
        let scopes = parse_scopes(&json);
        assert_eq!(scopes, vec![Scope::Read, Scope::Write]);
    }

    #[test]
    fn parse_scopes_ignores_unknown_values() {
        let json = serde_json::json!(["read", "superpower"]);
        let scopes = parse_scopes(&json);
        assert_eq!(scopes, vec![Scope::Read]);
    }

    #[test]
    fn parse_scopes_returns_empty_for_non_array() {
        let json = serde_json::json!("read");
        let scopes = parse_scopes(&json);
        assert!(scopes.is_empty());
    }

    #[test]
    fn scope_roundtrips_via_serde() {
        for scope in [Scope::Read, Scope::Write, Scope::Admin] {
            let s = serde_json::to_string(&scope).unwrap();
            let parsed: Scope = serde_json::from_str(&s).unwrap();
            assert_eq!(scope, parsed);
        }
    }

    #[test]
    fn scope_from_str_all_variants() {
        assert_eq!(Scope::from_str("read"), Some(Scope::Read));
        assert_eq!(Scope::from_str("write"), Some(Scope::Write));
        assert_eq!(Scope::from_str("admin"), Some(Scope::Admin));
        assert_eq!(Scope::from_str("unknown"), None);
    }

    #[test]
    fn require_scope_rejects_anonymous() {
        let auth = AuthContext::Anonymous;
        assert!(matches!(require_scope(&auth, &Scope::Read), Err(AppError::Unauthorized)));
    }

    #[test]
    fn require_scope_rejects_session_auth() {
        let auth = AuthContext::Session(SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: false,
        });
        assert!(matches!(require_scope(&auth, &Scope::Read), Err(AppError::Unauthorized)));
    }

    #[test]
    fn require_scope_accepts_matching_scope() {
        let info = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read, Scope::Write],
        };
        let auth = AuthContext::ApiKey(info);
        assert!(require_scope(&auth, &Scope::Read).is_ok());
        assert!(require_scope(&auth, &Scope::Write).is_ok());
    }

    #[test]
    fn require_scope_rejects_missing_scope() {
        let info = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        let auth = AuthContext::ApiKey(info);
        assert!(matches!(
            require_scope(&auth, &Scope::Write),
            Err(AppError::InsufficientScope)
        ));
    }

    #[test]
    fn require_verified_session_accepts_verified() {
        let info = make_session(true);
        let auth = AuthContext::Session(info.clone());
        let result = require_verified_session(auth).unwrap();
        assert_eq!(result.user_id, info.user_id);
    }

    #[test]
    fn require_verified_session_rejects_unverified_session() {
        let auth = AuthContext::Session(make_session(false));
        assert!(matches!(
            require_verified_session(auth),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn require_verified_session_rejects_anonymous() {
        assert!(matches!(
            require_verified_session(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }
}
