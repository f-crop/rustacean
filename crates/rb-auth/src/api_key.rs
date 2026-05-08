use std::fmt::Write as _;

use rand::RngCore as _;
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// API key with format `rb_live_<32hex>`.
///
/// The plaintext key is returned exactly once (on creation) and never stored.
/// Only its SHA-256 hex digest is persisted in the `api_keys` table.
/// No `Debug` or `Display` impl — prevents accidental logging of the secret.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct ApiKey(String);

impl ApiKey {
    /// Generate a new cryptographically-random API key.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 16];
        rand::rng().fill_bytes(&mut bytes);
        let mut hex = String::with_capacity(32);
        for b in &bytes {
            write!(hex, "{b:02x}").expect("infallible");
        }
        Self(format!("rb_live_{hex}"))
    }

    /// The full `rb_live_<32hex>` string for returning to the caller once.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// SHA-256 hex digest — this is what is stored in the database.
    #[must_use]
    pub fn hash(&self) -> String {
        let digest = Sha256::digest(self.0.as_bytes());
        format!("{digest:x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_key_has_rb_live_prefix() {
        let k = ApiKey::generate();
        assert!(
            k.as_str().starts_with("rb_live_"),
            "key must start with rb_live_"
        );
    }

    #[test]
    fn api_key_total_length_is_40() {
        let k = ApiKey::generate();
        // "rb_live_" (8) + 32 hex chars = 40
        assert_eq!(k.as_str().len(), 40);
    }

    #[test]
    fn api_key_hex_suffix_is_lowercase_hex() {
        let k = ApiKey::generate();
        let suffix = &k.as_str()[8..];
        assert!(
            suffix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        );
    }

    #[test]
    fn api_key_hash_is_64_hex_chars() {
        let k = ApiKey::generate();
        let h = k.hash();
        assert_eq!(h.len(), 64);
        assert!(h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
    }

    #[test]
    fn api_keys_are_unique() {
        let k1 = ApiKey::generate();
        let k2 = ApiKey::generate();
        assert_ne!(k1.as_str(), k2.as_str());
    }

    #[test]
    fn api_key_hash_is_deterministic_for_same_key() {
        let k = ApiKey::generate();
        let s = k.as_str().to_owned();
        let h1 = k.hash();
        // Simulate what the lookup path does: hash the raw string.
        let h2 = {
            let digest = Sha256::digest(s.as_bytes());
            format!("{digest:x}")
        };
        assert_eq!(h1, h2);
    }
}
