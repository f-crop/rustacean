use std::fmt::Write as _;

use rand::RngCore as _;
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Opaque 256-bit session token — base64url-encoded for transport.
///
/// The token itself is never stored; only its SHA-256 hex digest is persisted.
/// This type zeroizes on drop to prevent secrets from lingering in memory.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SessionToken(String);

impl SessionToken {
    /// Generate a new cryptographically-random session token.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let mut hex = String::with_capacity(64);
        for b in &bytes {
            write!(hex, "{b:02x}").expect("infallible");
        }
        Self(hex)
    }

    /// Raw token string, suitable for placement in `Set-Cookie`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// SHA-256 hex digest of the token — this is what is stored in the database.
    #[must_use]
    pub fn hash(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        format!("{digest:x}")
    }
}

/// Plaintext email verification token — 32 random bytes as lowercase hex.
///
/// The token itself is embedded in email URLs. Only its SHA-256 digest
/// is stored in the `email_tokens` table.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct EmailToken(String);

impl EmailToken {
    /// Generate a new cryptographically-random email token.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut bytes);
        let mut hex = String::with_capacity(64);
        for b in &bytes {
            write!(hex, "{b:02x}").expect("infallible");
        }
        Self(hex)
    }

    /// Plaintext token for embedding in URLs.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// SHA-256 hex digest — stored in `email_tokens.token_hash`.
    #[must_use]
    pub fn hash(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        format!("{digest:x}")
    }
}

/// Compute a SHA-256 hex digest of an arbitrary string slice.
/// Used to look up tokens presented by the caller.
#[must_use]
pub fn sha256_hex(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_token_length_is_64_hex_chars() {
        let t = SessionToken::generate();
        assert_eq!(t.as_str().len(), 64);
        assert!(
            t.as_str()
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
    }

    #[test]
    fn session_token_hash_is_64_hex_chars() {
        let t = SessionToken::generate();
        let h = t.hash();
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn session_tokens_are_unique() {
        let t1 = SessionToken::generate();
        let t2 = SessionToken::generate();
        assert_ne!(t1.as_str(), t2.as_str());
    }

    #[test]
    fn email_token_length_is_64_hex_chars() {
        let t = EmailToken::generate();
        assert_eq!(t.as_str().len(), 64);
    }

    #[test]
    fn sha256_hex_is_deterministic() {
        assert_eq!(sha256_hex("hello"), sha256_hex("hello"));
    }

    #[test]
    fn sha256_hex_differs_for_different_inputs() {
        assert_ne!(sha256_hex("hello"), sha256_hex("world"));
    }
}
