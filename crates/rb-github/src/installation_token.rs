//! GitHub installation-token minter (REQ-GH-05, ADR-005 §6.3).
//!
//! Implements [`TokenMinter`] by minting an App-JWT and exchanging it at
//! `POST /app/installations/{id}/access_tokens`. Used by [`crate::TokenCache`]
//! to fill misses; never called from the warm path.

use jsonwebtoken::EncodingKey;
use reqwest::Client;
use serde::Deserialize;

use crate::app_jwt::mint_app_jwt;
use crate::error::GhError;
use crate::secret::Secret;
use crate::token_cache::{CachedToken, MintFuture, TokenMinter};

const DEFAULT_BASE_URL: &str = "https://api.github.com";

/// Minter that hits the real GitHub REST API. Constructed inside
/// [`crate::GhApp::new`]; never instantiated by service code directly.
pub struct GitHubTokenMinter {
    app_id: i64,
    encoding_key: EncodingKey,
    http: Client,
    base_url: String,
}

impl GitHubTokenMinter {
    pub(crate) fn new(app_id: i64, encoding_key: EncodingKey, http: Client) -> Self {
        Self {
            app_id,
            encoding_key,
            http,
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }

    /// Create a minter that targets a custom API base URL.
    ///
    /// Intended for integration tests that stub the GitHub token endpoint with
    /// a local server (e.g. wiremock).  Gated behind the `test-helpers` feature
    /// so production builds are unaffected.
    #[cfg(feature = "test-helpers")]
    pub(crate) fn new_with_base_url(
        app_id: i64,
        encoding_key: EncodingKey,
        http: Client,
        base_url: String,
    ) -> Self {
        Self {
            app_id,
            encoding_key,
            http,
            base_url,
        }
    }
}

#[derive(Debug, Deserialize)]
struct InstallationTokenResponse {
    token: String,
    expires_at: chrono::DateTime<chrono::Utc>,
}

impl TokenMinter for GitHubTokenMinter {
    fn mint(&self, installation_id: i64) -> MintFuture<'_> {
        Box::pin(async move {
            let jwt = mint_app_jwt(self.app_id, &self.encoding_key)?;
            let url = format!(
                "{}/app/installations/{}/access_tokens",
                self.base_url, installation_id
            );
            let resp = self
                .http
                .post(&url)
                .header("Authorization", format!("Bearer {jwt}"))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .header("User-Agent", "rust-brain/1.0")
                .send()
                .await?;

            let status = resp.status().as_u16();
            if !resp.status().is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(GhError::ApiError { status, body });
            }

            let payload: InstallationTokenResponse = resp.json().await?;
            Ok(CachedToken {
                token: Secret::new(payload.token),
                expires_at: payload.expires_at,
            })
        })
    }
}
