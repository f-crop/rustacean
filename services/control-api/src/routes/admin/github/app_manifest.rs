//! `POST /v1/admin/github/app-manifest` — mint a manifest + state token and
//! return the GitHub "create app" redirect URL.

use axum::{Json, extract::State, response::IntoResponse};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use urlencoding::encode as urlencode;
use utoipa::ToSchema;

use crate::{
    error::AppError,
    middleware::{auth::AuthContext, platform_admin::require_platform_admin},
    state::AppState,
};

/// Body of `POST /v1/admin/github/app-manifest`.
///
/// `name` is operator-supplied (Q2: CTO 2026-05-12). When absent, defaults to
/// `rustacean-{RB_DEPLOYMENT_ID}` (fallback `rustacean-dev`).
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(default)]
pub struct AppManifestRequest {
    /// GitHub-facing App name — visible on the consent screen.
    pub name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppManifestResponse {
    /// `https://github.com/settings/apps/new?manifest=…&state=…` —
    /// platform admin should be redirected here.
    pub redirect_url: String,
    /// Opaque state token returned in the GitHub callback. Persisted as a
    /// sha256 hash; this hex string is single-use and short-lived.
    pub state_token: String,
}

#[utoipa::path(
    post,
    path = "/v1/admin/github/app-manifest",
    request_body = AppManifestRequest,
    responses(
        (status = 200, description = "Manifest redirect URL generated", body = AppManifestResponse),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Caller is not a platform admin"),
    ),
    tag = "admin"
)]
pub async fn post_app_manifest(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<AppManifestRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_platform_admin(&state.pool, auth).await?;

    let name = body
        .name
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_app_name);

    // 32 random bytes → hex for the URL, raw sha256 digest → BYTEA in the DB.
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let token_hex = hex::encode(raw);
    let token_hash: Vec<u8> = Sha256::digest(raw).to_vec();

    sqlx::query(
        "INSERT INTO control.github_manifest_states \
             (state_token_hash, initiated_by_user_id, expires_at) \
         VALUES ($1, $2, now() + interval '10 minutes')",
    )
    .bind(&token_hash)
    .bind(session.user_id)
    .execute(&state.pool)
    .await?;

    let manifest = build_manifest_payload(&state.config.base_url, &name);
    let manifest_json = serde_json::to_string(&manifest).map_err(|e| {
        tracing::error!(error = %e, "failed to serialize manifest payload");
        AppError::Internal(anyhow::anyhow!("manifest serialization failed"))
    })?;

    let redirect_url = format!(
        "https://github.com/settings/apps/new?manifest={}&state={}",
        urlencode(&manifest_json),
        token_hex
    );

    tracing::info!(
        user_id = %session.user_id,
        manifest_name = %name,
        "app-manifest: state token issued"
    );

    Ok(Json(AppManifestResponse {
        redirect_url,
        state_token: token_hex,
    }))
}

/// Build the App-creation manifest body. Public for testing.
#[must_use]
pub fn build_manifest_payload(base_url: &str, name: &str) -> serde_json::Value {
    json!({
        "name": name,
        "url": base_url,
        "redirect_url": format!("{base_url}/v1/admin/github/app-callback"),
        "hook_attributes": {
            "url": format!("{base_url}/v1/github/webhook"),
        },
        "callback_urls": [format!("{base_url}/v1/github/callback")],
        "public": false,
        "default_permissions": {
            "contents": "read",
            "metadata": "read",
        },
        "default_events": [],
    })
}

/// Default App name: `rustacean-{RB_DEPLOYMENT_ID}` or `rustacean-dev`.
fn default_app_name() -> String {
    let suffix = std::env::var("RB_DEPLOYMENT_ID")
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned());
    format!("rustacean-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_app_name_uses_deployment_id_when_set() {
        // SAFETY: tests are single-threaded for env access.
        unsafe { std::env::set_var("RB_DEPLOYMENT_ID", "prod-eu") };
        assert_eq!(default_app_name(), "rustacean-prod-eu");
        unsafe { std::env::remove_var("RB_DEPLOYMENT_ID") };
        assert_eq!(default_app_name(), "rustacean-dev");
    }

    #[test]
    fn manifest_payload_has_required_fields() {
        let m = build_manifest_payload("http://localhost:15173", "rustacean-test");
        assert_eq!(m["name"], "rustacean-test");
        assert_eq!(m["public"], false);
        assert_eq!(m["url"], "http://localhost:15173");
        assert_eq!(
            m["redirect_url"],
            "http://localhost:15173/v1/admin/github/app-callback"
        );
        assert_eq!(
            m["hook_attributes"]["url"],
            "http://localhost:15173/v1/github/webhook"
        );
        assert_eq!(m["default_permissions"]["contents"], "read");
        assert!(
            m["default_events"]
                .as_array()
                .expect("default_events array")
                .is_empty(),
            "installation/installation_repositories must not appear in default_events"
        );
    }
}
