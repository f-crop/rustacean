//! Postgres-backed `TokenStore` for OAuth Claude Code tokens (ADR-009 §7.5).
//!
//! OAuth refresh audit events are written to `audit.audit_events` at **100%** — no
//! sampling (RUSAA-861, security finding M-1). Both successes and failures are recorded
//! so anomalous refresh patterns (e.g. a compromised session refreshing from a second
//! IP) are always detectable.
//!
//! Tokens are stored AES-256-GCM encrypted (RUSAA-862).  This store decrypts them
//! after reading from Postgres and re-encrypts updated tokens before writing back.

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use rb_agent_runtime::{RuntimeError, TokenStore};

use crate::crypto::OauthTokenCipher;

const ANTHROPIC_TOKEN_URL: &str = "https://claude.ai/oauth/token";

/// Postgres-backed token store for the `claude_code` runtime.
pub struct PgTokenStore {
    pool: PgPool,
    http: reqwest::Client,
    client_id: String,
    /// Seconds of remaining validity below which a proactive refresh is triggered
    /// (controlled by `OAUTH_CLAUDE_TOKEN_REFRESH_LEAD_SECONDS`, default 60).
    refresh_lead_secs: i64,
    /// Current-key cipher for decrypt-on-read / encrypt-on-write.
    /// `None` means tokens are stored as plaintext (development only).
    cipher: Option<Arc<OauthTokenCipher>>,
}

impl PgTokenStore {
    pub fn new(
        pool: PgPool,
        http: reqwest::Client,
        client_id: String,
        refresh_lead_secs: i64,
        cipher: Option<Arc<OauthTokenCipher>>,
    ) -> Self {
        Self {
            pool,
            http,
            client_id,
            refresh_lead_secs,
            cipher,
        }
    }
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[async_trait]
impl TokenStore for PgTokenStore {
    async fn access_token(&self, tenant_id: Uuid, user_id: Uuid) -> Result<String, RuntimeError> {
        let row: Option<(String, Option<String>, Option<chrono::DateTime<Utc>>)> =
            sqlx::query_as(
                "SELECT access_token, refresh_token, expires_at \
                 FROM agents.oauth_tokens \
                 WHERE tenant_id = $1 AND user_id = $2 AND provider = 'claude_code'",
            )
            .bind(tenant_id)
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| RuntimeError::Internal(format!("DB error fetching OAuth token: {e}")))?;

        let (stored_access, stored_refresh, expires_at) = row.ok_or_else(|| {
            RuntimeError::TokenMissing {
                runtime_kind: "claude_code".to_owned(),
            }
        })?;

        // Decrypt the stored access token.
        let access_token = self.maybe_decrypt(&stored_access, user_id)?;

        let needs_refresh = expires_at
            .is_some_and(|exp| (exp - Utc::now()).num_seconds() < self.refresh_lead_secs);

        if !needs_refresh {
            return Ok(access_token);
        }

        let Some(stored_rt) = stored_refresh else {
            // No refresh token stored — treat as revoked.
            self.write_refresh_audit(tenant_id, user_id, "failure").await;
            return Err(RuntimeError::TokenMissing {
                runtime_kind: "claude_code".to_owned(),
            });
        };

        // Decrypt the refresh token before posting to Anthropic.
        let refresh_token = self.maybe_decrypt(&stored_rt, user_id)?;

        match self.do_refresh(&refresh_token, tenant_id, user_id).await {
            Ok(new_token) => {
                self.write_refresh_audit(tenant_id, user_id, "success").await;
                Ok(new_token)
            }
            Err(e) => {
                self.write_refresh_audit(tenant_id, user_id, "failure").await;
                Err(e)
            }
        }
    }
}

impl PgTokenStore {
    /// Decrypt a stored token value if a cipher is configured and the value is
    /// in encrypted format.  Returns the plaintext token string.
    fn maybe_decrypt(&self, stored: &str, user_id: Uuid) -> Result<String, RuntimeError> {
        match &self.cipher {
            Some(cipher) if OauthTokenCipher::is_encrypted(stored) => {
                cipher.decrypt(stored, user_id).map_err(|e| {
                    tracing::error!(user_id = %user_id, "OAuth token decryption failed: {e}");
                    RuntimeError::Internal("OAuth token decryption failed".to_owned())
                })
            }
            _ => Ok(stored.to_owned()),
        }
    }

    async fn do_refresh(
        &self,
        refresh_token: &str,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<String, RuntimeError> {
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", self.client_id.as_str()),
        ];

        let resp = self.http.post(ANTHROPIC_TOKEN_URL).form(&params).send().await?;

        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(RuntimeError::AnthropicApi { status, message: body });
        }

        let r: RefreshResponse = resp.json().await?;
        let new_expires_at = r.expires_in.map(|s| Utc::now() + chrono::Duration::seconds(s));

        // Encrypt new tokens before persistence.
        let (stored_access, stored_refresh, key_id) = match &self.cipher {
            Some(cipher) => {
                let enc_access = cipher
                    .encrypt(&r.access_token, user_id)
                    .map_err(|e| RuntimeError::Internal(format!("encrypt access_token: {e}")))?;
                let enc_refresh = r
                    .refresh_token
                    .as_deref()
                    .map(|rt| cipher.encrypt(rt, user_id))
                    .transpose()
                    .map_err(|e| {
                        RuntimeError::Internal(format!("encrypt refresh_token: {e}"))
                    })?;
                (enc_access, enc_refresh, cipher.key_id().to_owned())
            }
            None => (r.access_token.clone(), r.refresh_token.clone(), "none".to_owned()),
        };

        // Persist the refreshed tokens; warn on failure so the session can continue
        // with the new in-memory token even if the DB write is transient.
        let result = sqlx::query(
            "UPDATE agents.oauth_tokens \
             SET access_token      = $1, \
                 refresh_token     = COALESCE($2, refresh_token), \
                 expires_at        = $3, \
                 encryption_key_id = $4, \
                 updated_at        = now() \
             WHERE tenant_id = $5 AND user_id = $6 AND provider = 'claude_code'",
        )
        .bind(&stored_access)
        .bind(&stored_refresh)
        .bind(new_expires_at)
        .bind(&key_id)
        .bind(tenant_id)
        .bind(user_id)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            tracing::warn!(
                tenant_id = %tenant_id,
                user_id   = %user_id,
                "failed to persist refreshed OAuth token: {e}"
            );
        }

        Ok(r.access_token)
    }

    /// Write one refresh audit row to `audit.audit_events`.
    ///
    /// Called on **every** refresh attempt — successes and failures alike — at
    /// 100% rate (RUSAA-861). A failed write is logged at warn and silently
    /// dropped so that an audit hiccup never breaks the caller's token response.
    async fn write_refresh_audit(&self, tenant_id: Uuid, user_id: Uuid, outcome: &str) {
        let result = sqlx::query(
            "INSERT INTO audit.audit_events \
             (event_id, tenant_id, actor_kind, actor_user_id, action, outcome, occurred_at, payload) \
             VALUES ($1, $2, 'user', $3, 'oauth.claude.refresh', $4, now(), '{}') \
             ON CONFLICT (tenant_id, event_id) DO NOTHING",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(user_id)
        .bind(outcome)
        .execute(&self.pool)
        .await;

        if let Err(e) = result {
            tracing::warn!(
                tenant_id = %tenant_id,
                user_id   = %user_id,
                outcome,
                "failed to write OAuth refresh audit event: {e}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::Utc;

    fn needs_refresh(expires_at: Option<chrono::DateTime<Utc>>, lead_secs: i64) -> bool {
        expires_at
            .is_some_and(|exp| (exp - Utc::now()).num_seconds() < lead_secs)
    }

    #[test]
    fn refresh_triggered_when_expiry_within_lead() {
        let exp = Utc::now() + chrono::Duration::seconds(30);
        assert!(needs_refresh(Some(exp), 60));
    }

    #[test]
    fn no_refresh_when_expiry_beyond_lead() {
        let exp = Utc::now() + chrono::Duration::seconds(120);
        assert!(!needs_refresh(Some(exp), 60));
    }

    #[test]
    fn no_refresh_when_expiry_absent() {
        // expires_at = None → token has no expiry → no proactive refresh.
        assert!(!needs_refresh(None, 60));
    }

    #[test]
    fn refresh_at_exact_boundary_uses_strict_less_than() {
        // The condition is `remaining < lead_secs` (strict), so a token with
        // remaining > lead_secs must NOT trigger a refresh.
        // Use lead + 2 to stay clear of sub-second timing noise in CI.
        let exp = Utc::now() + chrono::Duration::seconds(62);
        assert!(!needs_refresh(Some(exp), 60));
    }

    #[test]
    fn refresh_lead_secs_zero_never_triggers() {
        // lead = 0 means only tokens already expired trigger refresh.
        let exp = Utc::now() + chrono::Duration::seconds(1);
        assert!(!needs_refresh(Some(exp), 0));
    }
}
