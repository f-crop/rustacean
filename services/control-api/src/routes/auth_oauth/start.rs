//! `GET /v1/auth/oauth/claude/start` — initiate PKCE OAuth flow for claude_code.
//!
//! Generates a random `code_verifier`, derives `code_challenge` (S256), and
//! stores both in a short-lived session cookie `rb_pkce_state` (max-age 600s).
//! Redirects the browser to the Anthropic OAuth consent screen.
//!
//! The optional `redirect_uri` query parameter controls where the browser is sent
//! after the callback completes.  It **must** share the same origin (scheme + host +
//! port) as `RB_BASE_URL`; requests with a foreign origin are rejected with
//! `400 bad_redirect_uri` (ADR-009 §6.3).

use axum::{
    extract::{Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
};
use base64::Engine as _;
use rand::Rng;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

const ANTHROPIC_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const PKCE_STATE_COOKIE: &str = "rb_pkce_state";

#[derive(Debug, Deserialize)]
pub struct StartQuery {
    /// Post-OAuth redirect destination.  Must share the same origin as `RB_BASE_URL`.
    pub redirect_uri: Option<String>,
}

/// `GET /v1/auth/oauth/claude/start` — begin Anthropic OAuth PKCE flow.
///
/// Responds with a 302 redirect to the Anthropic OAuth consent page.
/// Also sets the `rb_pkce_state` cookie containing
/// `{state}:{code_verifier}` or `{state}:{code_verifier}:{b64_redirect_uri}`.
#[utoipa::path(
    get,
    path = "/v1/auth/oauth/claude/start",
    params(
        ("redirect_uri" = Option<String>, Query, description = "Post-OAuth redirect destination (must share origin with RB_BASE_URL)"),
    ),
    responses(
        (status = 302, description = "Redirect to Anthropic OAuth consent screen"),
        (status = 400, description = "redirect_uri origin does not match RB_BASE_URL"),
        (status = 401, description = "Authentication required"),
    ),
    tag = "auth"
)]
pub async fn claude_oauth_start(
    State(state): State<AppState>,
    auth: AuthContext,
    Query(query): Query<StartQuery>,
) -> Result<Response, AppError> {
    require_verified_session(auth)?;

    if let Some(ref uri) = query.redirect_uri {
        if !same_origin(uri, &state.config.base_url) {
            tracing::warn!(redirect_uri = %uri, "redirect_uri origin rejected (open-redirect guard)");
            return Err(AppError::BadRedirectUri);
        }
    }

    let code_verifier = generate_code_verifier();
    let code_challenge = derive_code_challenge(&code_verifier);
    let oauth_state = generate_state();

    let client_id = state
        .config
        .claude_oauth_client_id
        .as_deref()
        .ok_or(AppError::RuntimeNotConfigured)?;

    let callback_uri = format!("{}/v1/auth/oauth/claude/callback", state.config.base_url);

    let auth_url = format!(
        "{ANTHROPIC_AUTH_URL}?response_type=code\
        &client_id={client_id}\
        &redirect_uri={}\
        &code_challenge={code_challenge}\
        &code_challenge_method=S256\
        &state={oauth_state}",
        urlencoding::encode(&callback_uri),
    );

    // Cookie format: "{state}:{code_verifier}" or "{state}:{code_verifier}:{b64_redirect}"
    // The redirect_uri is base64url-encoded so it contains no ':' and is cookie-safe.
    let cookie_value = match query.redirect_uri {
        Some(ref uri) => {
            let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(uri.as_bytes());
            format!("{oauth_state}:{code_verifier}:{b64}")
        }
        None => format!("{oauth_state}:{code_verifier}"),
    };

    let cookie = format!(
        "{PKCE_STATE_COOKIE}={cookie_value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600{}",
        if state.config.secure_cookies { "; Secure" } else { "" }
    );

    let mut resp = Redirect::temporary(&auth_url).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie)
            .map_err(|_| AppError::Internal(anyhow::anyhow!("internal error")))?,
    );

    Ok(resp)
}

// ---------------------------------------------------------------------------
// Origin validation (ADR-009 §6.3)
// ---------------------------------------------------------------------------

/// Returns `true` iff `candidate` shares the same origin as `allowed`.
///
/// Origin = scheme + "://" + host + optional port.  Both `candidate` and
/// `allowed` must use `http://` or `https://`; any other scheme returns `false`.
fn same_origin(candidate: &str, allowed: &str) -> bool {
    match (origin_of(candidate), origin_of(allowed)) {
        (Some(c), Some(a)) => c == a,
        _ => false,
    }
}

/// Extracts the origin prefix (`scheme://host[:port]`) from a URL string.
/// Returns `None` for non-HTTP/S schemes or malformed inputs.
fn origin_of(url: &str) -> Option<&str> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    let after_scheme = url.find("://")?;
    let authority_start = after_scheme + 3;
    let authority_end = url[authority_start..]
        .find('/')
        .map_or(url.len(), |i| authority_start + i);
    Some(&url[..authority_end])
}

// ---------------------------------------------------------------------------
// PKCE helpers
// ---------------------------------------------------------------------------

fn generate_code_verifier() -> String {
    let bytes: Vec<u8> = rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(64)
        .collect();
    String::from_utf8(bytes).expect("alphanumeric bytes are valid utf8")
}

fn derive_code_challenge(verifier: &str) -> String {
    let hash = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash)
}

fn generate_state() -> String {
    let bytes: Vec<u8> = rand::rng()
        .sample_iter(rand::distr::Alphanumeric)
        .take(32)
        .collect();
    String::from_utf8(bytes).expect("alphanumeric bytes are valid utf8")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_challenge_is_base64url_no_pad() {
        let verifier = "test_verifier_value_for_unit_test";
        let challenge = derive_code_challenge(verifier);
        assert!(!challenge.contains('+'));
        assert!(!challenge.contains('/'));
        assert!(!challenge.contains('='));
    }

    #[test]
    fn code_verifier_is_64_chars() {
        let v = generate_code_verifier();
        assert_eq!(v.len(), 64);
    }

    #[test]
    fn state_is_32_chars() {
        let s = generate_state();
        assert_eq!(s.len(), 32);
    }

    // origin_of tests
    #[test]
    fn origin_of_strips_path() {
        assert_eq!(origin_of("http://localhost:15173/foo/bar"), Some("http://localhost:15173"));
    }

    #[test]
    fn origin_of_no_path() {
        assert_eq!(origin_of("https://app.example.com"), Some("https://app.example.com"));
    }

    #[test]
    fn origin_of_with_port() {
        assert_eq!(origin_of("https://host:8443/path?q=1"), Some("https://host:8443"));
    }

    #[test]
    fn origin_of_rejects_non_http() {
        assert_eq!(origin_of("ftp://evil.com/"), None);
        assert_eq!(origin_of("javascript:alert(1)"), None);
    }

    // same_origin tests
    #[test]
    fn same_origin_exact_match() {
        assert!(same_origin("http://localhost:15173", "http://localhost:15173"));
    }

    #[test]
    fn same_origin_with_path() {
        assert!(same_origin(
            "http://localhost:15173/settings/integrations?oauth=claude",
            "http://localhost:15173"
        ));
    }

    #[test]
    fn same_origin_different_host_rejected() {
        assert!(!same_origin("http://evil.com/", "http://localhost:15173"));
    }

    #[test]
    fn same_origin_different_scheme_rejected() {
        assert!(!same_origin("https://localhost:15173", "http://localhost:15173"));
    }

    #[test]
    fn same_origin_different_port_rejected() {
        assert!(!same_origin("http://localhost:9999", "http://localhost:15173"));
    }

    #[test]
    fn same_origin_subdomain_rejected() {
        assert!(!same_origin("http://evil.localhost:15173", "http://localhost:15173"));
    }

    #[test]
    fn same_origin_with_port_in_redirect() {
        assert!(same_origin(
            "https://app.example.com:8443/callback",
            "https://app.example.com:8443"
        ));
    }
}
