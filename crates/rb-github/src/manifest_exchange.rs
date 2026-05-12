//! Exchange a one-time GitHub App Manifest `code` for live App credentials
//! (Phase 3 of the Manifest flow).
//!
//! GitHub's [Manifest flow](https://docs.github.com/en/apps/sharing-github-apps/registering-a-github-app-from-a-manifest)
//! redirects the operator to GitHub with a manifest blob; once they click
//! "Create app", GitHub bounces back to our callback with a single-use,
//! short-lived `code`. The callback then POSTs that code to
//! `https://api.github.com/app-manifests/{code}/conversions` to retrieve the
//! freshly-minted App's credentials.
//!
//! The `code` is single-use and expires in roughly one hour. Our callback
//! consumes it immediately and stores the resulting credentials in
//! `control.github_app_config` (Phase 1 schema) via
//! [`crate::AppConfigStore::insert_replacing`].

use reqwest::Client;
use serde::Deserialize;

use crate::error::GhError;

/// Default GitHub REST base URL. Overridden in tests by passing an explicit
/// base URL into [`exchange_manifest_code`].
pub const DEFAULT_GITHUB_API_BASE: &str = "https://api.github.com";

/// Subset of the manifest-conversion response that we persist. GitHub
/// returns additional fields (`name`, `owner`, `node_id`, `permissions`,
/// `events`) that the operator can re-read from GitHub's UI; we do not store
/// them.
#[derive(Debug, Clone, Deserialize)]
pub struct ManifestConversion {
    /// Numeric GitHub App ID — persisted to `github_app_config.app_id`.
    pub id: i64,
    /// URL-safe slug — used for building install URLs in the existing
    /// `/v1/github/install-url` route.
    pub slug: String,
    /// OAuth-style client identifier (`Iv1.…`).
    pub client_id: String,
    /// OAuth client secret. Stored encrypted.
    pub client_secret: String,
    /// HMAC-SHA256 shared secret GitHub will sign webhook deliveries with.
    /// Stored encrypted.
    pub webhook_secret: String,
    /// RSA PEM private key for App-level JWTs. Stored encrypted.
    pub pem: String,
}

/// Exchange a manifest `code` for the new App's credentials.
///
/// `base_url` is the GitHub REST base — production callers pass
/// [`DEFAULT_GITHUB_API_BASE`]; tests inject a wiremock stub address.
///
/// # Errors
///
/// Returns [`GhError::Http`] on transport failure, [`GhError::ApiError`]
/// when GitHub responds with a non-2xx status (the `code` was already used,
/// expired, or never existed), and [`GhError::JwtMint`] on JSON deserialization
/// failure — the latter is folded into the transport-error surface via the
/// `reqwest::Error::Decode` path.
pub async fn exchange_manifest_code(
    http: &Client,
    base_url: &str,
    code: &str,
) -> Result<ManifestConversion, GhError> {
    let url = format!("{base_url}/app-manifests/{code}/conversions");
    let resp = http
        .post(&url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(reqwest::header::USER_AGENT, "rustacean-control-api")
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(GhError::ApiError {
            status: status.as_u16(),
            body,
        });
    }

    let parsed: ManifestConversion = resp.json().await?;
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_minimal_payload() {
        let raw = r#"{
            "id": 12345,
            "slug": "demo-app",
            "client_id": "Iv1.demo",
            "client_secret": "cs",
            "webhook_secret": "ws",
            "pem": "-----BEGIN RSA PRIVATE KEY-----\n…\n-----END RSA PRIVATE KEY-----\n"
        }"#;
        let parsed: ManifestConversion = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.id, 12345);
        assert_eq!(parsed.slug, "demo-app");
        assert_eq!(parsed.client_id, "Iv1.demo");
    }

    #[test]
    fn deserializes_payload_with_extra_fields() {
        // GitHub returns name/owner/permissions/events; serde must ignore them.
        let raw = r#"{
            "id": 9,
            "slug": "x",
            "node_id": "MDEy",
            "name": "X",
            "owner": {"login": "octocat"},
            "client_id": "Iv1.x",
            "client_secret": "cs",
            "webhook_secret": "ws",
            "pem": "pem",
            "permissions": {"contents": "read"},
            "events": ["installation"]
        }"#;
        let parsed: ManifestConversion = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.id, 9);
    }
}
