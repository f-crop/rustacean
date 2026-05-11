//! Unit tests for [`super`] (i.e. `crate::middleware::auth`).
//!
//! Split into a sibling file via `#[cfg(test)] #[path = "auth_tests.rs"] mod tests`
//! to keep `auth.rs` itself within the project's 600-line file-size cap.

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

fn make_api_key(scopes: Vec<Scope>) -> ApiKeyInfo {
    ApiKeyInfo {
        key_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        scopes,
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
    assert_eq!(
        extract_bearer_token(&parts).as_deref(),
        Some("rb_live_abc123def456")
    );
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
    assert!(matches!(
        require_scope(&auth, &Scope::Read),
        Err(AppError::Unauthorized)
    ));
}

#[test]
fn require_scope_rejects_session_auth() {
    let auth = AuthContext::Session(SessionInfo {
        session_id: Uuid::new_v4(),
        user_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        email_verified: false,
    });
    assert!(matches!(
        require_scope(&auth, &Scope::Read),
        Err(AppError::Unauthorized)
    ));
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

#[test]
fn require_session_or_agent_key_accepts_verified_session() {
    let info = make_session(true);
    let expected_tenant = info.tenant_id;
    let expected_user = info.user_id;
    let auth = AuthContext::Session(info);
    let result = require_session_or_agent_key(auth).unwrap();
    assert_eq!(result.tenant_id, expected_tenant);
    assert_eq!(result.user_id, expected_user);
}

#[test]
fn require_session_or_agent_key_rejects_unverified_session() {
    let auth = AuthContext::Session(make_session(false));
    assert!(matches!(
        require_session_or_agent_key(auth),
        Err(AppError::EmailNotVerified)
    ));
}

#[test]
fn require_session_or_agent_key_rejects_expired_session() {
    assert!(matches!(
        require_session_or_agent_key(AuthContext::ExpiredSession),
        Err(AppError::SessionExpired)
    ));
}

#[test]
fn require_session_or_agent_key_accepts_agent_scoped_key() {
    let info = make_api_key(vec![Scope::Agent]);
    let expected_tenant = info.tenant_id;
    let expected_user = info.user_id;
    let auth = AuthContext::ApiKey(info);
    let result = require_session_or_agent_key(auth).unwrap();
    assert_eq!(result.tenant_id, expected_tenant);
    assert_eq!(result.user_id, expected_user);
}

#[test]
fn require_session_or_agent_key_accepts_agent_among_multiple_scopes() {
    let info = make_api_key(vec![Scope::Read, Scope::Agent]);
    let auth = AuthContext::ApiKey(info);
    assert!(require_session_or_agent_key(auth).is_ok());
}

#[test]
fn require_session_or_agent_key_rejects_read_only_key_with_insufficient_scope() {
    let auth = AuthContext::ApiKey(make_api_key(vec![Scope::Read]));
    assert!(matches!(
        require_session_or_agent_key(auth),
        Err(AppError::InsufficientScope)
    ));
}

#[test]
fn require_session_or_agent_key_rejects_admin_only_key_with_insufficient_scope() {
    let auth = AuthContext::ApiKey(make_api_key(vec![Scope::Admin]));
    assert!(matches!(
        require_session_or_agent_key(auth),
        Err(AppError::InsufficientScope)
    ));
}

#[test]
fn require_session_or_agent_key_rejects_write_only_key_with_insufficient_scope() {
    let auth = AuthContext::ApiKey(make_api_key(vec![Scope::Write]));
    assert!(matches!(
        require_session_or_agent_key(auth),
        Err(AppError::InsufficientScope)
    ));
}

#[test]
fn require_session_or_agent_key_rejects_scopeless_key_with_insufficient_scope() {
    let auth = AuthContext::ApiKey(make_api_key(vec![]));
    assert!(matches!(
        require_session_or_agent_key(auth),
        Err(AppError::InsufficientScope)
    ));
}

#[test]
fn require_session_or_agent_key_rejects_anonymous() {
    assert!(matches!(
        require_session_or_agent_key(AuthContext::Anonymous),
        Err(AppError::Unauthorized)
    ));
}
