mod api_key;
mod error;
mod hasher;
pub mod jwt;
pub mod redact;
mod rate_limiter;
mod token;

pub use api_key::ApiKey;
pub use error::AuthError;
pub use hasher::PasswordHasher;
pub use jwt::{JwtError, McpTokenClaims, MintedMcpClaims, mint_mcp_token, verify_mcp_token};
pub use rate_limiter::LoginRateLimiter;
pub use redact::{redact, redact_with_token};
pub use token::{EmailToken, SessionToken, sha256_hex};
