//! Short-lived MCP JWT minting and verification (ADR-013 §5).
//!
//! Tokens are HS256-signed, `aud="rb-mcp"`, tenant-bound, read-scoped.
//! They live in the runtime's `.mcp.json` only — never in prompts or logs.
//!
//! # Env vars consumed at the call site
//!
//! - `RB_MCP_JWT_SECRET` — HS256 signing secret (required when chat is enabled).
//! - `RB_MCP_JWT_TTL_SECS` — token lifetime in seconds (default 900 = 15 min).
//!
//! The caller (control-api) resolves these from `Config` / `AppState` and passes
//! them here.  This module is pure and has no I/O or env access.

use chrono::Utc;
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, TokenData, Validation, decode, encode,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum JwtError {
    #[error("JWT encoding failed: {0}")]
    Encode(#[from] jsonwebtoken::errors::Error),
    #[error("JWT is expired or invalid")]
    Invalid,
    #[error("JWT audience mismatch: expected rb-mcp")]
    AudienceMismatch,
}

// ---------------------------------------------------------------------------
// Claims
// ---------------------------------------------------------------------------

/// Claims carried by a minted MCP token (ADR-013 §5.2).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct McpTokenClaims {
    /// Chat session UUID (sub = session-scoped, not user-scoped).
    pub sub: Uuid,
    /// Server-trusted tenant binding — never accepted from tool args.
    pub tenant_id: Uuid,
    pub user_id: Uuid,
}

/// Full JWT payload for an MCP token (includes registered claims).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MintedMcpClaims {
    pub iss: String,
    pub aud: String,
    pub sub: String,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    /// Read-only scope.
    pub scope: Vec<String>,
    /// Token kind — `"human_chat"` for chat-session tokens (ADR-013 §5.2).
    /// Allows audit systems to distinguish chat traffic from autonomous-agent
    /// traffic without parsing the `sub` claim.
    pub kind: String,
    pub iat: i64,
    pub exp: i64,
    /// JWT ID for audit correlation and optional denylist.
    pub jti: String,
}

impl MintedMcpClaims {
    /// The `jti` (JWT ID) as a `Uuid` for convenience.
    #[must_use]
    pub fn jti_uuid(&self) -> Option<Uuid> {
        self.jti.parse().ok()
    }
}

// ---------------------------------------------------------------------------
// Mint
// ---------------------------------------------------------------------------

/// Mint a short-lived read-scoped MCP token for a chat session.
///
/// # Arguments
///
/// - `secret` — raw HS256 signing secret bytes.
/// - `ttl_secs` — token lifetime in seconds.
/// - `claims` — session-specific claims (`sub`/`tenant_id`/`user_id`).
///
/// # Errors
///
/// Returns [`JwtError::Encode`] if the signing step fails.
pub fn mint_mcp_token(
    secret: &[u8],
    ttl_secs: u64,
    claims: McpTokenClaims,
) -> Result<String, JwtError> {
    let now = Utc::now().timestamp();
    let payload = MintedMcpClaims {
        iss: "control-api".to_owned(),
        aud: "rb-mcp".to_owned(),
        sub: claims.sub.to_string(),
        tenant_id: claims.tenant_id,
        user_id: claims.user_id,
        scope: vec!["read".to_owned()],
        kind: "human_chat".to_owned(),
        iat: now,
        #[allow(clippy::cast_possible_wrap)]
        exp: now + ttl_secs as i64,
        jti: Uuid::new_v4().to_string(),
    };

    let header = Header::new(Algorithm::HS256);
    let key = EncodingKey::from_secret(secret);
    encode(&header, &payload, &key).map_err(JwtError::Encode)
}

// ---------------------------------------------------------------------------
// Verify
// ---------------------------------------------------------------------------

/// Verify a MCP JWT and return the parsed claims.
///
/// Checks signature, expiry, and `aud="rb-mcp"`.
///
/// # Errors
///
/// - [`JwtError::Invalid`] — expired, bad signature, or malformed.
/// - [`JwtError::AudienceMismatch`] — `aud` is not `"rb-mcp"`.
pub fn verify_mcp_token(token: &str, secret: &[u8]) -> Result<MintedMcpClaims, JwtError> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_audience(&["rb-mcp"]);
    validation.set_issuer(&["control-api"]);
    // No grace period: short-lived MCP tokens must be rejected the moment they
    // expire. The default 60s leeway is wrong for ≤15 min chat-session tokens.
    validation.leeway = 0;

    let key = DecodingKey::from_secret(secret);
    let data: TokenData<MintedMcpClaims> =
        decode(token, &key, &validation).map_err(|_| JwtError::Invalid)?;

    if data.claims.aud != "rb-mcp" {
        return Err(JwtError::AudienceMismatch);
    }

    Ok(data.claims)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &[u8] = b"test-mcp-jwt-secret-must-be-long-enough-for-hs256"; // gitleaks:allow — test-only HS256 fixture, not a real secret
    const TTL: u64 = 900;

    fn make_claims() -> McpTokenClaims {
        McpTokenClaims {
            sub: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
        }
    }

    #[test]
    fn round_trip() {
        let claims = make_claims();
        let tenant_id = claims.tenant_id;
        let token = mint_mcp_token(SECRET, TTL, claims).unwrap();
        let decoded = verify_mcp_token(&token, SECRET).unwrap();
        assert_eq!(decoded.tenant_id, tenant_id);
        assert_eq!(decoded.aud, "rb-mcp");
        assert_eq!(decoded.iss, "control-api");
        assert_eq!(decoded.scope, vec!["read"]);
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = mint_mcp_token(SECRET, TTL, make_claims()).unwrap();
        let err = verify_mcp_token(&token, b"wrong-secret").unwrap_err();
        assert!(matches!(err, JwtError::Invalid));
    }

    #[test]
    fn expired_token_is_rejected() {
        let token = mint_mcp_token(SECRET, 0, make_claims()).unwrap();
        // TTL=0 ⇒ exp==iat. jsonwebtoken's check is now > exp; wait one second
        // so that now' > exp is true and the token is correctly rejected.
        std::thread::sleep(std::time::Duration::from_secs(1));
        let err = verify_mcp_token(&token, SECRET).unwrap_err();
        assert!(matches!(err, JwtError::Invalid));
    }

    #[test]
    fn jti_is_valid_uuid() {
        let token = mint_mcp_token(SECRET, TTL, make_claims()).unwrap();
        let decoded = verify_mcp_token(&token, SECRET).unwrap();
        assert!(decoded.jti_uuid().is_some());
    }

    #[test]
    fn scope_is_read_only() {
        let token = mint_mcp_token(SECRET, TTL, make_claims()).unwrap();
        let decoded = verify_mcp_token(&token, SECRET).unwrap();
        assert_eq!(decoded.scope, vec!["read"]);
        assert!(!decoded.scope.contains(&"write".to_owned()));
    }

    #[test]
    fn kind_is_human_chat() {
        let token = mint_mcp_token(SECRET, TTL, make_claims()).unwrap();
        let decoded = verify_mcp_token(&token, SECRET).unwrap();
        assert_eq!(decoded.kind, "human_chat");
    }
}
