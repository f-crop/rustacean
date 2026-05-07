//! `GET /v1/auth/oauth/claude/start` — initiate PKCE OAuth flow for claude_code.
//!
//! Generates a random `code_verifier`, derives `code_challenge` (S256), and
//! stores both in a short-lived session cookie `rb_pkce_state` (max-age 600s).
//! Redirects the browser to the Anthropic OAuth consent screen.

use axum::{
    extract::State,
    http::{HeaderValue, header},
    response::{IntoResponse, Redirect, Response},
};
use rand::Rng;
use sha2::{Digest, Sha256};
use base64::Engine as _;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_verified_session},
    state::AppState,
};

const ANTHROPIC_AUTH_URL: &str = "https://claude.ai/oauth/authorize";
const PKCE_STATE_COOKIE: &str = "rb_pkce_state";

/// `GET /v1/auth/oauth/claude/start` — begin Anthropic OAuth PKCE flow.
///
/// Responds with a 302 redirect to the Anthropic OAuth consent page.
/// Also sets the `rb_pkce_state` cookie containing `{state}:{code_verifier}`.
#[utoipa::path(
    get,
    path = "/v1/auth/oauth/claude/start",
    responses(
        (status = 302, description = "Redirect to Anthropic OAuth consent screen"),
        (status = 401, description = "Authentication required"),
    ),
    tag = "auth"
)]
pub async fn claude_oauth_start(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<Response, AppError> {
    require_verified_session(auth)?;

    let code_verifier = generate_code_verifier();
    let code_challenge = derive_code_challenge(&code_verifier);
    let oauth_state = generate_state();

    let client_id = state
        .config
        .claude_oauth_client_id
        .as_deref()
        .ok_or(AppError::RuntimeNotConfigured)?;

    let redirect_uri = format!("{}/v1/auth/oauth/claude/callback", state.config.base_url);

    let auth_url = format!(
        "{ANTHROPIC_AUTH_URL}?response_type=code\
        &client_id={client_id}\
        &redirect_uri={}\
        &code_challenge={code_challenge}\
        &code_challenge_method=S256\
        &state={oauth_state}",
        urlencoding::encode(&redirect_uri),
    );

    // Store verifier + state in a short-lived cookie so the callback can retrieve it.
    let cookie_value = format!("{oauth_state}:{code_verifier}");
    let cookie = format!(
        "{PKCE_STATE_COOKIE}={cookie_value}; HttpOnly; SameSite=Lax; Path=/; Max-Age=600{}",
        if state.config.secure_cookies { "; Secure" } else { "" }
    );

    let mut resp = Redirect::temporary(&auth_url).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|_| AppError::Internal(anyhow::anyhow!("internal error")))?,
    );

    Ok(resp)
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
}
