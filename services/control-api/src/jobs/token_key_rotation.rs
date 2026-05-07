//! Background job: re-encrypt `agents.oauth_tokens` rows with a new KMS key.
//!
//! # When this runs
//!
//! Spawned as a one-shot tokio task at startup when `RB_OAUTH_ROTATE_KEYS_ON_BOOT=true`
//! *and* a previous key (`RB_OAUTH_ENCRYPT_KEY_PREV`) is configured, or when the
//! table contains rows with `encryption_key_id = 'none'` (legacy plaintext).
//!
//! # What it does
//!
//! 1. Queries `agents.oauth_tokens` WHERE `encryption_key_id != current_key_id` in
//!    batches of [`BATCH_SIZE`] rows (uses the partial index from migration 012).
//! 2. For each row:
//!    - `encryption_key_id = 'none'` → token is plaintext; encrypt with current key.
//!    - any other value → decrypt with the previous key, re-encrypt with current key.
//! 3. Updates the row atomically (`access_token`, `refresh_token`,
//!    `encryption_key_id`, `updated_at`).
//! 4. Emits Prometheus counters and structured log lines at each batch boundary.
//!
//! # Metrics
//!
//! - `oauth_key_rotation_rows_rotated_total` — rows successfully re-encrypted.
//! - `oauth_key_rotation_rows_failed_total` — rows that could not be re-encrypted
//!   (logged as ERROR; rotation continues so partial failures are recoverable).

use std::sync::Arc;

use sqlx::PgPool;
use uuid::Uuid;

use crate::crypto::OauthTokenCipher;

const BATCH_SIZE: i64 = 50;

/// Re-encrypt all `agents.oauth_tokens` rows whose `encryption_key_id` differs
/// from `current.key_id()`.
///
/// - `prev` must be `Some` if the table contains rows encrypted with a retired key.
///   Rows with `encryption_key_id = 'none'` (plaintext legacy) do not need `prev`.
///
/// Returns the count of successfully re-encrypted rows.  Errors on individual
/// rows are logged and counted but do not abort the sweep.
pub async fn rotate_oauth_token_keys(
    pool: &PgPool,
    current: &Arc<OauthTokenCipher>,
    prev: Option<&Arc<OauthTokenCipher>>,
) -> u64 {
    let target_key_id = current.key_id().to_owned();
    let mut total_ok: u64 = 0;
    let mut total_err: u64 = 0;

    loop {
        let rows: Vec<(Uuid, Uuid, String, Option<String>, String)> = match sqlx::query_as(
            "SELECT id, user_id, access_token, refresh_token, encryption_key_id \
             FROM agents.oauth_tokens \
             WHERE encryption_key_id != $1 \
             LIMIT $2",
        )
        .bind(&target_key_id)
        .bind(BATCH_SIZE)
        .fetch_all(pool)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("token_key_rotation: DB error reading batch: {e}");
                break;
            }
        };

        if rows.is_empty() {
            break;
        }

        let batch_len = rows.len();

        for (id, user_id, access_token, refresh_token, old_key_id) in &rows {
            let result = reencrypt_row(
                *id,
                *user_id,
                access_token,
                refresh_token.as_deref(),
                old_key_id,
                current,
                prev.map(Arc::as_ref),
                pool,
            )
            .await;

            match result {
                Ok(()) => {
                    total_ok += 1;
                    metrics::counter!("oauth_key_rotation_rows_rotated_total").increment(1);
                }
                Err(e) => {
                    total_err += 1;
                    metrics::counter!("oauth_key_rotation_rows_failed_total").increment(1);
                    tracing::error!(
                        row_id = %id,
                        user_id = %user_id,
                        old_key_id,
                        "token_key_rotation: failed to rotate row: {e}"
                    );
                }
            }
        }

        tracing::info!(
            batch = batch_len,
            ok = total_ok,
            errors = total_err,
            "token_key_rotation: batch complete"
        );
    }

    tracing::info!(
        ok = total_ok,
        errors = total_err,
        "token_key_rotation: sweep finished"
    );
    total_ok
}

#[allow(clippy::too_many_arguments)]
async fn reencrypt_row(
    id: Uuid,
    user_id: Uuid,
    access_token: &str,
    refresh_token: Option<&str>,
    old_key_id: &str,
    current: &OauthTokenCipher,
    prev: Option<&OauthTokenCipher>,
    pool: &PgPool,
) -> anyhow::Result<()> {
    let new_access = reencrypt_value(access_token, user_id, old_key_id, current, prev)?;
    let new_refresh = refresh_token
        .map(|rt| reencrypt_value(rt, user_id, old_key_id, current, prev))
        .transpose()?;

    sqlx::query(
        "UPDATE agents.oauth_tokens \
         SET access_token    = $1, \
             refresh_token   = $2, \
             encryption_key_id = $3, \
             updated_at      = now() \
         WHERE id = $4",
    )
    .bind(&new_access)
    .bind(new_refresh.as_deref())
    .bind(current.key_id())
    .bind(id)
    .execute(pool)
    .await
    .map_err(|e| anyhow::anyhow!("DB update failed for row {id}: {e}"))?;

    Ok(())
}

fn reencrypt_value(
    stored: &str,
    user_id: Uuid,
    old_key_id: &str,
    current: &OauthTokenCipher,
    prev: Option<&OauthTokenCipher>,
) -> anyhow::Result<String> {
    let plaintext = if old_key_id == "none" {
        // Legacy plaintext row — no decryption needed.
        stored.to_owned()
    } else if let Some(p) = prev {
        p.decrypt(stored, user_id)
            .map_err(|e| anyhow::anyhow!("decrypt with prev key '{old_key_id}' failed: {e}"))?
    } else {
        anyhow::bail!(
            "row encrypted with key_id='{old_key_id}' but RB_OAUTH_ENCRYPT_KEY_PREV is not set; \
             configure the previous key before running the rotation sweep"
        );
    };

    current
        .encrypt(&plaintext, user_id)
        .map_err(|e| anyhow::anyhow!("re-encrypt with current key failed: {e}"))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use super::reencrypt_value;
    use crate::crypto::OauthTokenCipher;

    fn cipher(id: &str) -> Arc<OauthTokenCipher> {
        // Use distinct key bytes so current != prev.
        let hex = if id == "oauth-claude-v1" {
            "01".repeat(32)
        } else {
            "02".repeat(32)
        };
        Arc::new(OauthTokenCipher::from_hex(id, &hex).unwrap())
    }

    #[test]
    fn reencrypt_plaintext_row() {
        let current = cipher("oauth-claude-v1");
        let uid = Uuid::new_v4();
        let result = reencrypt_value("raw_token", uid, "none", &current, None).unwrap();
        assert!(OauthTokenCipher::is_encrypted(&result));
        assert_eq!(current.decrypt(&result, uid).unwrap(), "raw_token");
    }

    #[test]
    fn reencrypt_prev_key_row() {
        let current = cipher("oauth-claude-v1");
        let prev = cipher("oauth-claude-v0");
        let uid = Uuid::new_v4();

        // Encrypt with prev key first.
        let old_enc = prev.encrypt("secret", uid).unwrap();

        // Re-encrypt with current key.
        let result =
            reencrypt_value(&old_enc, uid, "oauth-claude-v0", &current, Some(&prev)).unwrap();

        // Decryptable with current key only.
        assert_eq!(current.decrypt(&result, uid).unwrap(), "secret");
        // No longer decryptable with prev key (different ciphertext).
        assert!(prev.decrypt(&result, uid).is_err());
    }

    #[test]
    fn missing_prev_key_for_encrypted_row_is_error() {
        let current = cipher("oauth-claude-v1");
        let uid = Uuid::new_v4();
        let err = reencrypt_value("v1:some_blob", uid, "oauth-claude-v0", &current, None);
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("RB_OAUTH_ENCRYPT_KEY_PREV"));
    }
}
