use reqwest::Client;
use serde::Deserialize;

use crate::{GhApp, app_jwt::mint_app_jwt, error::GhError};

#[derive(Debug, Deserialize)]
pub struct AppIdentity {
    pub id: i64,
    pub slug: String,
    pub owner: AppOwner,
}

#[derive(Debug, Deserialize)]
pub struct AppOwner {
    pub login: String,
}

#[derive(Debug)]
pub struct RepoInfo {
    pub full_name: String,
    pub default_branch: String,
}

#[derive(Deserialize)]
struct GhRepoResponse {
    full_name: String,
    default_branch: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallationInfo {
    pub id: i64,
    pub account: InstallationAccount,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InstallationAccount {
    pub login: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub id: i64,
}

pub async fn fetch_app_identity(app: &GhApp, http: &Client) -> Result<AppIdentity, GhError> {
    let jwt = mint_app_jwt(app.app_id, &app.encoding_key)?;
    let resp = http
        .get("https://api.github.com/app")
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
    Ok(resp.json::<AppIdentity>().await?)
}

pub(crate) async fn fetch_repo_by_id(
    http: &Client,
    installation_token: &str,
    repo_id: i64,
) -> Result<RepoInfo, GhError> {
    let url = format!("https://api.github.com/repositories/{repo_id}");
    let resp = http
        .get(&url)
        .header("Authorization", format!("Bearer {installation_token}"))
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
    let raw = resp.json::<GhRepoResponse>().await?;
    Ok(RepoInfo {
        full_name: raw.full_name,
        default_branch: raw.default_branch,
    })
}

pub async fn fetch_installation(
    app: &GhApp,
    http: &Client,
    installation_id: i64,
) -> Result<InstallationInfo, GhError> {
    let jwt = mint_app_jwt(app.app_id, &app.encoding_key)?;
    let resp = http
        .get(format!(
            "https://api.github.com/app/installations/{installation_id}"
        ))
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
    Ok(resp.json::<InstallationInfo>().await?)
}
