//! Storage layer for the platform-admin–registered GitHub App credentials
//! (Phase 1 of the Manifest flow).
//!
//! Persists encrypted GitHub App credentials in `control.github_app_config`.
//! Phase 1 only ships the store; the per-request `GhApp` loader that consumes
//! these rows lands in Phase 2.
//!
//! ## Encryption
//!
//! AES-256-GCM with a per-row random 12-byte nonce. The key is read from
//! `RB_GH_APP_ENC_KEY` (base64 of exactly 32 bytes) and is dedicated to GitHub
//! App secrets — independent from `RB_TOKEN_ENC_KEY` so the two rotation
//! lifetimes do not couple. The active `encryption_key_id` is recorded on the
//! row (`'gh-app-v1'` today) so a future rotation job can target stale rows
//! without a table scan.
//!
//! ## Singleton-active invariant
//!
//! Migration `017_github_app_config.sql` enforces a partial unique index
//! `((1)) WHERE is_active`, so at most one row may be active. [`AppConfigStore::insert_replacing`]
//! runs a transactional `UPDATE ... SET is_active=false` ahead of the `INSERT`
//! to preserve the invariant when a new App replaces the current one.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, Key, KeyInit, Nonce};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use jsonwebtoken::EncodingKey;
use rand::RngCore as _;
use sqlx::PgPool;
use uuid::Uuid;

use crate::GhApp;
use crate::error::GhError;
use crate::secret::Secret;

/// Active key identifier recorded on every newly written row. Future rotation
/// flips the constant and re-encrypts rows in the background.
pub const CURRENT_ENCRYPTION_KEY_ID: &str = "gh-app-v1";

/// Nonce length for AES-256-GCM (96 bits, the AEAD-recommended size).
const NONCE_LEN: usize = 12;

/// Errors raised by the store. Storage and crypto failures are returned
/// distinctly so callers can surface 503 vs. 500 appropriately.
#[derive(Debug, thiserror::Error)]
pub enum AppConfigError {
    /// `RB_GH_APP_ENC_KEY` env var is unset or empty.
    #[error(
        "RB_GH_APP_ENC_KEY is required to read or write github_app_config. \
         Set it to a base64-encoded 32-byte AES-256 key before enabling the \
         Manifest flow."
    )]
    KeyMissing,

    /// `RB_GH_APP_ENC_KEY` is not valid base64.
    #[error("RB_GH_APP_ENC_KEY is not valid base64: {0}")]
    KeyNotBase64(#[from] base64::DecodeError),

    /// Decoded key bytes are not exactly 32 bytes.
    #[error("RB_GH_APP_ENC_KEY must decode to exactly 32 bytes, got {0}")]
    KeyWrongLength(usize),

    /// AES-GCM encrypt/decrypt failure (tag mismatch, malformed input, etc.).
    /// The inner aead error is intentionally not surfaced — it varies across
    /// failure modes and is not actionable to the caller.
    #[error("github_app_config crypto failure")]
    Crypto,

    /// Postgres / sqlx failure.
    #[error("github_app_config storage error: {0}")]
    Db(#[from] sqlx::Error),

    /// Stored row records an `encryption_key_id` we do not have a key for.
    /// Surfaces during rollouts where the key id was bumped without rotation.
    #[error("github_app_config row encrypted with unknown key id {0}")]
    UnknownKeyId(String),

    /// Stored nonce bytes are not the expected length.
    #[error("github_app_config row has malformed {0} nonce ({1} bytes)")]
    MalformedNonce(&'static str, usize),
}

/// AES-256-GCM key material derived from `RB_GH_APP_ENC_KEY`.
///
/// The key is stored inside the cipher and never re-exposed. Cloning the
/// struct clones the underlying cipher — cheap (`Aes256Gcm` is `Clone`-able
/// via its key schedule reference) and safe to share across tasks.
#[derive(Clone)]
pub struct EncryptionKey {
    cipher: Aes256Gcm,
    key_id: &'static str,
}

impl std::fmt::Debug for EncryptionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptionKey")
            .field("key_id", &self.key_id)
            .field("cipher", &"[REDACTED]")
            .finish()
    }
}

impl EncryptionKey {
    /// Load the active key from `RB_GH_APP_ENC_KEY`.
    ///
    /// # Errors
    ///
    /// Returns [`AppConfigError::KeyMissing`] when the env var is unset,
    /// [`AppConfigError::KeyNotBase64`] when the value is malformed, or
    /// [`AppConfigError::KeyWrongLength`] when the decoded byte length is
    /// not exactly 32 bytes.
    pub fn from_env() -> Result<Self, AppConfigError> {
        let raw = std::env::var("RB_GH_APP_ENC_KEY")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or(AppConfigError::KeyMissing)?;
        Self::from_base64(&raw)
    }

    /// Decode a base64-encoded 32-byte key. Exposed for tests; production
    /// callers use [`EncryptionKey::from_env`].
    ///
    /// # Errors
    ///
    /// Returns [`AppConfigError::KeyNotBase64`] when the value is malformed,
    /// or [`AppConfigError::KeyWrongLength`] when the decoded byte length is
    /// not exactly 32 bytes.
    pub fn from_base64(b64: &str) -> Result<Self, AppConfigError> {
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.trim())?;
        if bytes.len() != 32 {
            return Err(AppConfigError::KeyWrongLength(bytes.len()));
        }
        let key = Key::<Aes256Gcm>::from_slice(&bytes);
        Ok(Self {
            cipher: Aes256Gcm::new(key),
            key_id: CURRENT_ENCRYPTION_KEY_ID,
        })
    }

    /// Identifier persisted in `encryption_key_id` for rows this key produces.
    #[must_use]
    pub fn key_id(&self) -> &'static str {
        self.key_id
    }

    /// Encrypt `plaintext`. Returns `(nonce, ciphertext)` for storage. A fresh
    /// 12-byte nonce is generated on every call.
    fn seal(&self, plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), AppConfigError> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ct = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| AppConfigError::Crypto)?;
        Ok((nonce_bytes.to_vec(), ct))
    }

    fn open(&self, nonce_bytes: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>, AppConfigError> {
        if nonce_bytes.len() != NONCE_LEN {
            return Err(AppConfigError::MalformedNonce("aes-gcm", nonce_bytes.len()));
        }
        let nonce = Nonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| AppConfigError::Crypto)
    }
}

/// New-row inserts: caller passes plaintext credentials. The store handles
/// encryption and the `is_active` swap.
#[derive(Debug, Clone)]
pub struct NewAppConfig {
    pub app_id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub private_key_pem: Secret<String>,
    pub webhook_secret: Secret<String>,
}

/// Decrypted view of an `github_app_config` row.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub id: i64,
    pub app_id: i64,
    pub slug: String,
    pub client_id: String,
    pub client_secret: Secret<String>,
    pub private_key_pem: Secret<String>,
    pub webhook_secret: Secret<String>,
    pub encryption_key_id: String,
    pub installed_by_user_id: Uuid,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub deactivated_at: Option<DateTime<Utc>>,
}

/// Build a live `GhApp` from a decrypted [`AppConfig`].
///
/// Used by Phase 2's startup `GhAppLoader` seeding path and by the Phase 3
/// register/replace callback to hot-swap a new App into the loader.
///
/// # Errors
///
/// Returns [`GhError::InvalidKey`] when the stored PEM bytes cannot be parsed
/// as an RSA private key.
pub fn try_build_gh_app(cfg: &AppConfig) -> Result<GhApp, GhError> {
    let pem = cfg.private_key_pem.expose().as_bytes();
    let encoding_key = EncodingKey::from_rsa_pem(pem)
        .map_err(|e| GhError::InvalidKey(format!("github_app_config row {}: {e}", cfg.id)))?;
    let webhook_secret = Secret::new(cfg.webhook_secret.expose().as_bytes().to_vec());
    Ok(GhApp::new(cfg.app_id, encoding_key, webhook_secret))
}

/// Postgres-backed store for the App-config table.
///
/// The store is cheap to clone — it holds a `PgPool` (already `Arc`-wrapped
/// internally) and a key-material handle.
#[derive(Clone)]
pub struct AppConfigStore {
    pool: PgPool,
    key: EncryptionKey,
}

impl AppConfigStore {
    #[must_use]
    pub fn new(pool: PgPool, key: EncryptionKey) -> Self {
        Self { pool, key }
    }

    /// Return the currently-active row, if any.
    ///
    /// # Errors
    ///
    /// Returns [`AppConfigError::Db`] on transport failures and
    /// [`AppConfigError::Crypto`] / [`AppConfigError::UnknownKeyId`] when a
    /// row cannot be decrypted with the current key material.
    pub async fn load_active(&self) -> Result<Option<AppConfig>, AppConfigError> {
        let row: Option<EncryptedRow> = sqlx::query_as::<_, EncryptedRow>(
            "SELECT id, app_id, slug, client_id, \
                    client_secret_ciphertext, client_secret_nonce, \
                    private_key_ciphertext, private_key_nonce, \
                    webhook_secret_ciphertext, webhook_secret_nonce, \
                    encryption_key_id, installed_by_user_id, is_active, \
                    created_at, deactivated_at \
               FROM github_app_config \
              WHERE is_active = true \
              LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| self.decrypt_row(r)).transpose()
    }

    /// Insert a new active row, deactivating any existing active row in the
    /// same transaction. Returns the freshly inserted row's id.
    ///
    /// # Errors
    ///
    /// Returns [`AppConfigError::Crypto`] when sealing the secrets fails, or
    /// [`AppConfigError::Db`] on transport / constraint failures (the
    /// singleton partial unique index should never fire because the
    /// deactivation runs in the same transaction).
    pub async fn insert_replacing(
        &self,
        new: NewAppConfig,
        installed_by_user_id: Uuid,
    ) -> Result<i64, AppConfigError> {
        let (cs_nonce, cs_ct) = self.key.seal(new.client_secret.expose().as_bytes())?;
        let (pk_nonce, pk_ct) = self.key.seal(new.private_key_pem.expose().as_bytes())?;
        let (ws_nonce, ws_ct) = self.key.seal(new.webhook_secret.expose().as_bytes())?;

        let mut tx = self.pool.begin().await?;

        sqlx::query(
            "UPDATE github_app_config \
                SET is_active = false, deactivated_at = now() \
              WHERE is_active = true",
        )
        .execute(&mut *tx)
        .await?;

        let id: (i64,) = sqlx::query_as(
            "INSERT INTO github_app_config \
                 (app_id, slug, client_id, \
                  client_secret_ciphertext, client_secret_nonce, \
                  private_key_ciphertext, private_key_nonce, \
                  webhook_secret_ciphertext, webhook_secret_nonce, \
                  encryption_key_id, installed_by_user_id, is_active) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, true) \
             RETURNING id",
        )
        .bind(new.app_id)
        .bind(&new.slug)
        .bind(&new.client_id)
        .bind(&cs_ct)
        .bind(&cs_nonce)
        .bind(&pk_ct)
        .bind(&pk_nonce)
        .bind(&ws_ct)
        .bind(&ws_nonce)
        .bind(self.key.key_id())
        .bind(installed_by_user_id)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(id.0)
    }

    fn decrypt_row(&self, row: EncryptedRow) -> Result<AppConfig, AppConfigError> {
        if row.encryption_key_id != self.key.key_id() {
            return Err(AppConfigError::UnknownKeyId(row.encryption_key_id));
        }
        let cs = self
            .key
            .open(&row.client_secret_nonce, &row.client_secret_ciphertext)?;
        let pk = self
            .key
            .open(&row.private_key_nonce, &row.private_key_ciphertext)?;
        let ws = self
            .key
            .open(&row.webhook_secret_nonce, &row.webhook_secret_ciphertext)?;
        Ok(AppConfig {
            id: row.id,
            app_id: row.app_id,
            slug: row.slug,
            client_id: row.client_id,
            client_secret: Secret::new(String::from_utf8(cs).map_err(|_| AppConfigError::Crypto)?),
            private_key_pem: Secret::new(
                String::from_utf8(pk).map_err(|_| AppConfigError::Crypto)?,
            ),
            webhook_secret: Secret::new(String::from_utf8(ws).map_err(|_| AppConfigError::Crypto)?),
            encryption_key_id: row.encryption_key_id,
            installed_by_user_id: row.installed_by_user_id,
            is_active: row.is_active,
            created_at: row.created_at,
            deactivated_at: row.deactivated_at,
        })
    }
}

#[derive(sqlx::FromRow)]
struct EncryptedRow {
    id: i64,
    app_id: i64,
    slug: String,
    client_id: String,
    client_secret_ciphertext: Vec<u8>,
    client_secret_nonce: Vec<u8>,
    private_key_ciphertext: Vec<u8>,
    private_key_nonce: Vec<u8>,
    webhook_secret_ciphertext: Vec<u8>,
    webhook_secret_nonce: Vec<u8>,
    encryption_key_id: String,
    installed_by_user_id: Uuid,
    is_active: bool,
    created_at: DateTime<Utc>,
    deactivated_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_key() -> EncryptionKey {
        let bytes = [7u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        EncryptionKey::from_base64(&b64).expect("from_base64")
    }

    #[test]
    fn seal_open_roundtrip() {
        let key = fresh_key();
        let plaintext = b"super-secret-pem-bytes";
        let (nonce, ct) = key.seal(plaintext).expect("seal");
        assert_eq!(nonce.len(), NONCE_LEN);
        assert_ne!(ct.as_slice(), plaintext, "ciphertext must differ");
        let recovered = key.open(&nonce, &ct).expect("open");
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn seal_uses_unique_nonce_per_call() {
        let key = fresh_key();
        let (n1, _) = key.seal(b"x").expect("seal 1");
        let (n2, _) = key.seal(b"x").expect("seal 2");
        assert_ne!(n1, n2, "two seals must produce distinct nonces");
    }

    #[test]
    fn open_rejects_tampered_ciphertext() {
        let key = fresh_key();
        let (nonce, mut ct) = key.seal(b"data").expect("seal");
        ct[0] ^= 0x01;
        let err = key.open(&nonce, &ct).expect_err("must fail");
        assert!(matches!(err, AppConfigError::Crypto));
    }

    #[test]
    fn open_rejects_wrong_nonce_length() {
        let key = fresh_key();
        let (_, ct) = key.seal(b"data").expect("seal");
        let err = key.open(&[0u8; 8], &ct).expect_err("must fail");
        assert!(matches!(err, AppConfigError::MalformedNonce("aes-gcm", 8)));
    }

    #[test]
    fn from_base64_rejects_wrong_length() {
        let short = base64::engine::general_purpose::STANDARD.encode([0u8; 16]);
        let err = EncryptionKey::from_base64(&short).expect_err("must fail");
        assert!(matches!(err, AppConfigError::KeyWrongLength(16)));
    }

    #[test]
    fn from_base64_rejects_garbage() {
        let err = EncryptionKey::from_base64("not base64!!!").expect_err("must fail");
        assert!(matches!(err, AppConfigError::KeyNotBase64(_)));
    }

    #[test]
    fn from_env_reads_var_and_missing_returns_key_missing() {
        // Combined so the env-var read and clear cannot race with another test
        // touching the same variable in parallel.
        let bytes = [3u8; 32];
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        // SAFETY: scoped to this test; cleaned up before exit.
        unsafe { std::env::set_var("RB_GH_APP_ENC_KEY", &b64) };
        let key = EncryptionKey::from_env().expect("from_env when set");
        assert_eq!(key.key_id(), CURRENT_ENCRYPTION_KEY_ID);
        // SAFETY: same scope.
        unsafe { std::env::remove_var("RB_GH_APP_ENC_KEY") };
        let err = EncryptionKey::from_env().expect_err("must fail when unset");
        assert!(matches!(err, AppConfigError::KeyMissing));
    }

    #[test]
    fn debug_does_not_leak_key_bytes() {
        let key = fresh_key();
        let dbg = format!("{key:?}");
        assert!(dbg.contains("REDACTED"));
        assert!(!dbg.contains('7'));
    }

    /// Minimal 2048-bit RSA private key fixture for `try_build_gh_app` tests.
    /// Generated once with `openssl genrsa 2048` and committed here so the
    /// test does not shell out at runtime.
    const TEST_RSA_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEowIBAAKCAQEAtm5VskdYukSx2KsOZ24Sb1m+EtFsi3PtvR58dxhcN4UISKtm\n\
WjE+wXymvNkN0YHaZJjJzo2Y/N79Zhxn1m6Ywda4ZWAJV3IIYZbk6BByv8mhVgGQ\n\
1qFTsCdH/CdMzqj+vfk3rvf3YyMaJOZ/+xtmVKMHcmGtFMu8MDmZbeP1aanCkVm8\n\
OF4FvOe9D2POgFEFfwT89U7uppF+ATSx9fEt1/QcQUTrLNimRJh7HwoXFKlhfFAj\n\
LH80hCfQRl4Wd5DwqqDDC4VxOXyf3HxxBxxV8jzPNCsCmIIuB6t05DZAuMTOLcKy\n\
JFP6JpBn3SVtAR7w3MEs99/x3qjC4OlGEbR6IQIDAQABAoIBABf1Olqi1DwzAEHa\n\
mDXSx5gjFC0HrkjpIRFVnmwlfV1+8sNCmJYf2BBO27qXVxOTNkQqXrLnDA10v8XB\n\
SkJl3Y3kQTNAqJYUW5G1XSyAJlhbtP+CADxBQH/wYjphE5Ynna8tehDh3WiTYzAt\n\
3LtSlcEUKR1+EQfdJ8KGzQXEFOWiF/IzTtJsKlcc78ARtO27NhFRG6+lzeUaq2T7\n\
9TFiAjcEatGEWY9YEPpoZdtYTwHHKwHkj8x5y5+ZBSXlMm5DKZsa+v5K9hL2vy2X\n\
QcOZjMfemfnPctbpyaZqZyApdkBhFRG9SgGnQiHHcDdAH5+Knsap3vNYz8VLXNXz\n\
4DLBfdECgYEA56iE60lDDeyHkXjOQyG3hcrTwXKWMOJlIohatuVdKzqWyzWFFcGz\n\
1WIcsmGOxx39NUiNmTpgQv4i7VL2njBSahPgcyAdQDg6/+ePyytQHwlqqJpu2Pzz\n\
qrFOQbxFLN2RtSdoZmcRiXKDpaIvDsiLBcLM8E/AUVx7Ux/i8ohRkrUCgYEAyfei\n\
Ek7+Ovju5OdbDC3SY8KrwLJ4QGZZl+ehfNkUEMKKt3JFkbCDX4PuyJ+UNuiB9PV3\n\
8ZxoaCAjGgK0wIM4SHa4PUtP+TBjpMlR3HVlEH3yvFsHfTwzx/Cab9IT7nfWZGq+\n\
Y67O+SQILUiPVa0+sxudARMTRwjzKE7y3M6gcv0CgYByeWzrSLA9k+UlFKi/zCRk\n\
HfQbDqLY5wkidwQc9o0vNkPGqrn3ZZkBdsBPyqIRZS+/d6PucGV8AKtVZj9DwhgM\n\
T0udD+EBjN3jKx3IejNMOg4SegzbgRBR9HQt3WkVZqIPLMzkc4xt62urUyMOg/W+\n\
zR4OXC9c++FoxxIugTCpdQKBgQCQ1Ed+JqDQ+CMmK+IjnAo1IXMqDQ4cFL10sJsR\n\
n+vGGY9hxlGzbE4HX3Z/5pHFL3oQEgkVgsa05Aa7+sb0OFczQ9oKR9P+IM+x9MMr\n\
TLAm9ZIpKjrlOM4ja2zNXOcVbnvFwdRgxlNGAU5cKQpZbVDp5YXJaQRkbU1IUlnz\n\
y96luQKBgGHF9XLI5tFdMxQE3pjyaHe7Tt5VYV6f8K56nC4Iqfdki/IFiBg83p3R\n\
zVa6Vv1iEhPwwm/PV/zVScVqxX2nJOOoa4Lk7dlPTu62Onki++YdsGGUtV+gC2bM\n\
EkrxIBjqfn0FWqzC2WhRzeTLE+xq0NHcCS7vJOzvSqLNqUtmvE0t\n\
-----END RSA PRIVATE KEY-----\n";

    fn fixture_app_config() -> AppConfig {
        AppConfig {
            id: 1,
            app_id: 9001,
            slug: "fixture-app".to_owned(),
            client_id: "Iv1.fixture".to_owned(),
            client_secret: Secret::new("client-secret".to_owned()),
            private_key_pem: Secret::new(TEST_RSA_PEM.to_owned()),
            webhook_secret: Secret::new("hook-secret".to_owned()),
            encryption_key_id: CURRENT_ENCRYPTION_KEY_ID.to_owned(),
            installed_by_user_id: Uuid::nil(),
            is_active: true,
            created_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).expect("epoch"),
            deactivated_at: None,
        }
    }

    #[test]
    fn try_build_gh_app_constructs_from_valid_pem() {
        let cfg = fixture_app_config();
        let app = try_build_gh_app(&cfg).expect("build");
        assert_eq!(app.app_id, 9001);
    }

    #[test]
    fn try_build_gh_app_rejects_garbage_pem() {
        let mut cfg = fixture_app_config();
        cfg.private_key_pem = Secret::new("not a pem".to_owned());
        let err = try_build_gh_app(&cfg).expect_err("must fail");
        match err {
            GhError::InvalidKey(msg) => {
                assert!(msg.contains("github_app_config row 1"));
            }
            other => panic!("expected InvalidKey, got {other:?}"),
        }
    }
}
