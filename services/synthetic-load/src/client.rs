use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Thin wrapper around `reqwest::Client` that carries a session cookie jar.
///
/// Cookie-based session is established by calling `login()`. On 401 the
/// caller is expected to re-login and retry.
#[derive(Clone, Debug)]
pub struct ApiClient {
    pub client: Client,
    pub base_url: String,
    /// Trace ID of the most recent failed request, saved for daily summaries.
    pub last_failed_trace_id: Option<String>,
}

impl ApiClient {
    pub fn new(base_url: &str) -> Result<Self> {
        let client = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(30))
            .user_agent("synthetic-load/0.1 rustbrain-harness")
            .build()
            .context("failed to build reqwest client")?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            last_failed_trace_id: None,
        })
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn post_json<B: Serialize, R: for<'de> Deserialize<'de>>(
        &mut self,
        path: &str,
        body: &B,
    ) -> Result<R> {
        let resp = self
            .client
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path} network error"))?;

        let status = resp.status();
        if !status.is_success() {
            if let Some(v) = resp.headers().get("x-trace-id") {
                if let Ok(s) = v.to_str() {
                    self.last_failed_trace_id = Some(s.to_owned());
                }
            }
        }
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("POST {path} → {status}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("POST {path}: deserialize failed: {text}"))
    }

    pub async fn get_json<R: for<'de> Deserialize<'de>>(&mut self, path: &str) -> Result<R> {
        let resp = self
            .client
            .get(self.url(path))
            .send()
            .await
            .with_context(|| format!("GET {path} network error"))?;

        let status = resp.status();
        if !status.is_success() {
            if let Some(v) = resp.headers().get("x-trace-id") {
                if let Ok(s) = v.to_str() {
                    self.last_failed_trace_id = Some(s.to_owned());
                }
            }
        }
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("GET {path} → {status}: {text}");
        }
        serde_json::from_str(&text)
            .with_context(|| format!("GET {path}: deserialize failed: {text}"))
    }

    pub async fn delete_ok(&mut self, path: &str) -> Result<StatusCode> {
        let resp = self
            .client
            .delete(self.url(path))
            .send()
            .await
            .with_context(|| format!("DELETE {path} network error"))?;
        Ok(resp.status())
    }

    /// Returns `(up: bool, error_detail: Option<String>)`.
    pub async fn health_check(&self, service_url: &str) -> (bool, Option<String>) {
        let url = format!("{}/health", service_url.trim_end_matches('/'));
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => (true, None),
            Ok(r) => (
                false,
                Some(format!("{service_url} health → {}", r.status())),
            ),
            Err(e) => (false, Some(format!("{service_url} health error: {e}"))),
        }
    }

    /// Returns the SHA reported by `/health/build`, or `None` on failure.
    pub async fn health_build_sha(&self, service_url: &str) -> Option<String> {
        #[derive(Deserialize)]
        struct BuildResp {
            sha: String,
        }
        let url = format!("{}/health/build", service_url.trim_end_matches('/'));
        let resp = self
            .client
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .ok()?;
        resp.json::<BuildResp>().await.ok().map(|b| b.sha)
    }
}

// ---------------------------------------------------------------------------
// Shared request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct SignupRequest {
    pub email: String,
    pub password: String,
    pub tenant_name: String,
}

#[derive(Debug, Deserialize)]
pub struct SignupResponse {
    pub user_id: Uuid,
}

#[derive(Debug, Serialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginResponse {}

#[derive(Debug, Serialize)]
pub struct CreateSessionRequest {
    pub runtime: String,
    pub initial_prompt: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionResponse {
    pub session_id: Uuid,
    pub status: String,
}

/// Response from `GET /v1/agents/sessions/{id}`.
#[derive(Debug, Deserialize)]
pub struct SessionDetail {
    pub status: String,
}

/// Response from `POST /v1/repos/{repo_id}/ingestions`.
#[derive(Debug, Deserialize)]
pub struct TriggerIngestionResponse {
    pub ingest_run_id: Uuid,
    pub trace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RepoItem {
    pub repo_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct RepoListResponse {
    pub repos: Vec<RepoItem>,
}

#[derive(Debug, Deserialize)]
pub struct StageRunItem {
    pub stage: String,
    pub status: String,
    pub error_message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StageTimelineResponse {
    pub stages: Vec<StageRunItem>,
}

#[derive(Debug, Serialize)]
pub struct SearchRequest {
    pub q: String,
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SearchResponse {}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` when an `anyhow::Error` from a client method indicates HTTP 401.
pub fn is_unauthorized(err: &anyhow::Error) -> bool {
    err.to_string().contains("→ 401")
}
