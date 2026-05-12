mod app_config_store;
mod app_jwt;
mod client;
mod error;
mod installation_token;
mod loader;
mod repos;
mod secret;
mod state_token;
mod token_cache;
mod webhook;

pub use app_config_store::{
    AppConfig, AppConfigError, AppConfigStore, CURRENT_ENCRYPTION_KEY_ID, EncryptionKey,
    NewAppConfig, try_build_gh_app,
};
pub use client::{AppIdentity, AppOwner, InstallationInfo, RepoInfo};
pub use error::GhError;
pub use loader::GhAppLoader;
pub use repos::{RepoItem, RepoPage};
pub use secret::Secret;
pub use state_token::hash_token;
pub use token_cache::{CachedToken, MintFuture, SAFETY_MARGIN, TokenCache, TokenMinter};
pub use webhook::{
    Account, Installation, InstallationEvent, InstallationPayload, InstallationReposPayload,
    InstallationRepositoriesEvent, ReplayCache, RepoRef, verify_signature,
};

use std::sync::Arc;
use std::time::Duration;

use jsonwebtoken::EncodingKey;
use moka::future::Cache;
use reqwest::Client;

use installation_token::GitHubTokenMinter;

/// The central handle for all GitHub App operations.
///
/// Constructed once at startup, stored in `AppState`, and cloned cheaply
/// because all inner fields are `Arc`-wrapped.
#[derive(Clone)]
pub struct GhApp {
    pub app_id: i64,
    /// Opaque RS256 key; never exposed beyond this crate.
    pub(crate) encoding_key: EncodingKey,
    /// Raw webhook secret bytes. Wrapped in Secret so it never logs.
    pub(crate) webhook_secret: Secret<Vec<u8>>,
    /// Replay protection cache for `X-GitHub-Delivery` UUIDs.
    pub replay_cache: ReplayCache,
    /// Cached response from `GET /app` (60 s TTL) to avoid hammering GitHub
    /// from k8s liveness probes.
    identity_cache: Arc<Cache<(), AppIdentity>>,
    /// Per-installation access-token cache (REQ-GH-05).
    pub token_cache: Arc<TokenCache>,
    /// Shared HTTP client.
    http: Client,
}

impl GhApp {
    /// # Panics
    ///
    /// Does not panic in practice; `reqwest::Client::builder().build()` only
    /// fails with an invalid TLS configuration, which cannot occur with the
    /// default builder.
    #[must_use]
    pub fn new(app_id: i64, encoding_key: EncodingKey, webhook_secret: Secret<Vec<u8>>) -> Self {
        let identity_cache = Cache::builder()
            .max_capacity(1)
            .time_to_live(Duration::from_secs(60))
            .build();
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("reqwest client init is infallible with valid config");
        let minter: Arc<dyn TokenMinter> = Arc::new(GitHubTokenMinter::new(
            app_id,
            encoding_key.clone(),
            http.clone(),
        ));
        let token_cache = TokenCache::new(minter);
        Self {
            app_id,
            encoding_key,
            webhook_secret,
            replay_cache: ReplayCache::new(),
            identity_cache: Arc::new(identity_cache),
            token_cache,
            http,
        }
    }

    /// Verifies a GitHub webhook signature header against the raw body.
    ///
    /// # Errors
    ///
    /// Returns [`GhError::BadSignatureFormat`] or [`GhError::SignatureMismatch`].
    pub fn verify_webhook(&self, body: &[u8], sig_header: &str) -> Result<(), GhError> {
        webhook::verify::verify_signature(body, sig_header, &self.webhook_secret)
    }

    /// Returns the GitHub App's identity, using a 60-second cache so
    /// `/v1/health/github-app` is safe to call from liveness probes.
    ///
    /// # Errors
    ///
    /// Returns [`GhError`] if the App JWT cannot be minted or the GitHub API
    /// call fails.
    pub async fn check_identity(&self) -> Result<AppIdentity, GhError> {
        if let Some(cached) = self.identity_cache.get(&()).await {
            return Ok(cached);
        }
        let identity = client::fetch_app_identity(self, &self.http).await?;
        self.identity_cache.insert((), identity).await;
        self.identity_cache
            .get(&())
            .await
            .ok_or_else(|| GhError::ApiError {
                status: 500,
                body: "identity cache miss immediately after insert".to_owned(),
            })
    }

    /// Returns a usable installation access token, minting if absent or
    /// near expiry (REQ-GH-05). Warm hits are sub-millisecond.
    ///
    /// # Errors
    ///
    /// Returns [`GhError`] if the App JWT cannot be minted or the GitHub
    /// `POST /app/installations/{id}/access_tokens` call fails.
    pub async fn installation_token(
        &self,
        installation_id: i64,
    ) -> Result<Secret<String>, GhError> {
        self.token_cache.get_or_mint(installation_id).await
    }

    /// Fetches a repository by GitHub numeric ID, confirming it is accessible
    /// via the given installation.
    ///
    /// # Errors
    ///
    /// Returns [`GhError::ApiError { status: 404, .. }`] when the repo does not
    /// exist or is not accessible through this installation. Other GitHub API
    /// errors and token-minting failures propagate as [`GhError`].
    pub async fn fetch_repo(
        &self,
        installation_id: i64,
        repo_id: i64,
    ) -> Result<RepoInfo, GhError> {
        let token = self.installation_token(installation_id).await?;
        client::fetch_repo_by_id(&self.http, token.expose(), repo_id).await
    }

    /// Returns the paginated list of repositories accessible to an installation.
    ///
    /// # Errors
    ///
    /// Returns [`GhError`] if the installation token cannot be minted or the
    /// GitHub API call fails.
    pub async fn list_installation_repos(
        &self,
        installation_id: i64,
        page: u32,
        per_page: u32,
    ) -> Result<repos::RepoPage, GhError> {
        let token = self.token_cache.get_or_mint(installation_id).await?;
        repos::list_installation_repos(token.expose(), &self.http, page, per_page).await
    }

    /// Fetches metadata for a GitHub App installation by installation ID.
    ///
    /// # Errors
    ///
    /// Returns [`GhError`] if the App JWT cannot be minted or the GitHub API
    /// call fails.
    pub async fn fetch_installation(
        &self,
        installation_id: i64,
    ) -> Result<client::InstallationInfo, GhError> {
        client::fetch_installation(self, &self.http, installation_id).await
    }

    /// Spawns the periodic eviction sweep for the installation-token cache.
    /// Must be called from inside a tokio runtime, typically once at server
    /// startup right after [`GhApp::new`].
    pub fn start_token_sweep(&self) {
        self.token_cache.start_sweep();
    }
}

impl std::fmt::Debug for GhApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhApp")
            .field("app_id", &self.app_id)
            .field("encoding_key", &"[REDACTED]")
            .field("webhook_secret", &self.webhook_secret)
            .finish_non_exhaustive()
    }
}

impl Clone for AppIdentity {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            slug: self.slug.clone(),
            owner: AppOwner {
                login: self.owner.login.clone(),
            },
        }
    }
}
