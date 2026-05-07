//! AES-256-GCM token cipher for OAuth refresh/access token encryption at rest.
//!
//! Design (ADR-009 §7.5):
//! - Master key loaded from `RB_OAUTH_ENCRYPT_KEY` (hex-encoded 32 bytes).
//! - Per-user subkey derived via HKDF-SHA-256(ikm=master, salt=user_id, info=key_id).
//! - Ciphertext format: `"v1:<base64(12-byte-nonce || aes-gcm-ciphertext)>"`.
//!   The `v1:` prefix allows future format migrations and identifies legacy
//!   plaintext rows (no prefix) for the rotation sweep.
//! - Key ID (e.g. `"oauth-claude-v1"`) is stored in `oauth_tokens.encryption_key_id`
//!   so the rotation job can find rows encrypted with a retired key.

use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, KeyInit},
};
use base64::Engine as _;
use hkdf::Hkdf;
use rand::RngCore as _;
use sha2::Sha256;
use uuid::Uuid;
use zeroize::ZeroizeOnDrop;

const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// AES-256-GCM token cipher with per-user HKDF key derivation.
///
/// Holds a 32-byte master key in memory; zeroizes on drop.
/// Wrap in [`std::sync::Arc`] when sharing across request handlers.
#[derive(ZeroizeOnDrop)]
pub struct OauthTokenCipher {
    key_id: String,
    key_material: [u8; KEY_LEN],
}

impl std::fmt::Debug for OauthTokenCipher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Intentionally omit key_material to avoid accidental logging.
        f.debug_struct("OauthTokenCipher")
            .field("key_id", &self.key_id)
            .field("key_material", &"[REDACTED]")
            .finish()
    }
}

/// Errors returned by [`OauthTokenCipher`] operations.
#[derive(Debug, thiserror::Error)]
pub enum CipherError {
    #[error("invalid key hex: {0}")]
    InvalidKeyHex(String),
    #[error("key must be exactly 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    #[error("invalid ciphertext format (expected v1:<base64>)")]
    InvalidFormat,
    #[error("AEAD decryption failed (wrong key or corrupted ciphertext)")]
    DecryptFailed,
}

impl OauthTokenCipher {
    /// Construct from a hex-encoded 32-byte master key.
    ///
    /// `key_id` is the logical KMS key label (e.g. `"oauth-claude-v1"`) stored
    /// in `oauth_tokens.encryption_key_id` to enable rotation tracking.
    ///
    /// # Errors
    ///
    /// Returns [`CipherError::InvalidKeyHex`] if `hex_key` is not valid hex, or
    /// [`CipherError::InvalidKeyLength`] if the decoded length is not 32.
    pub fn from_hex(key_id: impl Into<String>, hex_key: &str) -> Result<Self, CipherError> {
        let bytes = hex::decode(hex_key).map_err(|e| CipherError::InvalidKeyHex(e.to_string()))?;
        let n = bytes.len();
        if n != KEY_LEN {
            return Err(CipherError::InvalidKeyLength(n));
        }
        let mut key_material = [0u8; KEY_LEN];
        key_material.copy_from_slice(&bytes);
        Ok(Self { key_id: key_id.into(), key_material })
    }

    /// The KMS key id label for this cipher instance.
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Returns `true` if `value` was produced by [`encrypt`][Self::encrypt]
    /// (starts with `"v1:"`).  Values without this prefix are legacy plaintext.
    pub fn is_encrypted(value: &str) -> bool {
        value.starts_with("v1:")
    }

    /// Encrypt `plaintext` for `user_id`.
    ///
    /// Derives a per-user subkey via HKDF, then encrypts with AES-256-GCM
    /// using a freshly generated 96-bit nonce.  Returns `"v1:<base64>"`.
    ///
    /// # Errors
    ///
    /// Returns [`CipherError::DecryptFailed`] on the unlikely AEAD failure path.
    pub fn encrypt(&self, plaintext: &str, user_id: Uuid) -> Result<String, CipherError> {
        let subkey = self.derive_subkey(user_id);
        let cipher = Aes256Gcm::new_from_slice(&subkey).expect("32-byte key always valid");

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);

        // AES-GCM encrypt never fails in practice for valid key/nonce; map to
        // our error type for the API contract.
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| CipherError::DecryptFailed)?;

        let mut blob = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        blob.extend_from_slice(&nonce_bytes);
        blob.extend_from_slice(&ciphertext);

        Ok(format!(
            "v1:{}",
            base64::engine::general_purpose::STANDARD.encode(&blob)
        ))
    }

    /// Decrypt a value produced by [`encrypt`][Self::encrypt].
    ///
    /// # Errors
    ///
    /// - [`CipherError::InvalidFormat`] — value does not start with `"v1:"` or
    ///   the base64 / blob length is invalid.
    /// - [`CipherError::DecryptFailed`] — AEAD tag verification failed (wrong
    ///   key, wrong user_id, or corrupted ciphertext).
    pub fn decrypt(&self, encoded: &str, user_id: Uuid) -> Result<String, CipherError> {
        let b64 = encoded.strip_prefix("v1:").ok_or(CipherError::InvalidFormat)?;
        let blob = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|_| CipherError::InvalidFormat)?;

        if blob.len() < NONCE_LEN {
            return Err(CipherError::InvalidFormat);
        }

        let nonce = aes_gcm::Nonce::from_slice(&blob[..NONCE_LEN]);
        let subkey = self.derive_subkey(user_id);
        let cipher = Aes256Gcm::new_from_slice(&subkey).expect("32-byte key always valid");

        let plaintext_bytes = cipher
            .decrypt(nonce, &blob[NONCE_LEN..])
            .map_err(|_| CipherError::DecryptFailed)?;

        String::from_utf8(plaintext_bytes).map_err(|_| CipherError::InvalidFormat)
    }

    /// HKDF-SHA-256(ikm=master_key, salt=user_id_bytes, info=key_id_bytes) → 32-byte subkey.
    ///
    /// Binding the subkey to `user_id` means a leaked column value is useless
    /// without both the master key and the specific user's UUID.
    fn derive_subkey(&self, user_id: Uuid) -> [u8; KEY_LEN] {
        let hk = Hkdf::<Sha256>::new(Some(user_id.as_bytes()), &self.key_material);
        let mut out = [0u8; KEY_LEN];
        hk.expand(self.key_id.as_bytes(), &mut out)
            .expect("HKDF expand to 32 bytes always succeeds");
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn test_key_hex() -> String {
        // 32 zero bytes — valid but only for tests.
        "00".repeat(32)
    }

    fn cipher() -> OauthTokenCipher {
        OauthTokenCipher::from_hex("oauth-claude-v1", &test_key_hex()).unwrap()
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let c = cipher();
        let uid = Uuid::new_v4();
        let plaintext = "super-secret-refresh-token";

        let enc = c.encrypt(plaintext, uid).unwrap();
        assert!(OauthTokenCipher::is_encrypted(&enc));
        assert!(enc.starts_with("v1:"));

        let dec = c.decrypt(&enc, uid).unwrap();
        assert_eq!(dec, plaintext);
    }

    #[test]
    fn different_nonce_each_call() {
        let c = cipher();
        let uid = Uuid::new_v4();
        let e1 = c.encrypt("token", uid).unwrap();
        let e2 = c.encrypt("token", uid).unwrap();
        // Probabilistically different (1 in 2^96 chance of collision).
        assert_ne!(e1, e2);
    }

    #[test]
    fn wrong_user_id_fails_decrypt() {
        let c = cipher();
        let uid1 = Uuid::new_v4();
        let uid2 = Uuid::new_v4();
        let enc = c.encrypt("token", uid1).unwrap();
        assert!(c.decrypt(&enc, uid2).is_err());
    }

    #[test]
    fn tampered_ciphertext_fails_decrypt() {
        let c = cipher();
        let uid = Uuid::new_v4();
        let mut enc = c.encrypt("token", uid).unwrap();
        // Flip a byte in the base64 payload after the "v1:" prefix.
        let payload = &mut enc["v1:".len()..];
        let mut bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .unwrap();
        bytes[12] ^= 0xFF; // flip a ciphertext byte (after the nonce)
        let tampered = format!(
            "v1:{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        assert!(c.decrypt(&tampered, uid).is_err());
    }

    #[test]
    fn plaintext_not_detected_as_encrypted() {
        assert!(!OauthTokenCipher::is_encrypted("ya29.raw_access_token"));
        assert!(!OauthTokenCipher::is_encrypted(""));
    }

    #[test]
    fn invalid_key_hex_rejected() {
        assert!(OauthTokenCipher::from_hex("k", "not-hex").is_err());
    }

    #[test]
    fn wrong_key_length_rejected() {
        let short_hex = "0011aabb"; // 4 bytes
        let err = OauthTokenCipher::from_hex("k", short_hex).unwrap_err();
        assert!(matches!(err, CipherError::InvalidKeyLength(4)));
    }

    #[test]
    fn is_encrypted_requires_v1_prefix() {
        assert!(OauthTokenCipher::is_encrypted("v1:AAEC"));
        assert!(!OauthTokenCipher::is_encrypted("v2:AAEC"));
        assert!(!OauthTokenCipher::is_encrypted("AAEC"));
    }
}
