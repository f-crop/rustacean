//! `POST /v1/search` — semantic code search (REQ-DP-01).
//!
//! Embeds the caller's query via Ollama, searches the `rb_embeddings` Qdrant
//! collection within the caller's tenant scope, and returns ranked code symbols.
//!
//! Multi-tenancy is enforced at two layers:
//!   1. [`TenantVectorStore::search`] injects a `must` `tenant_id` filter so
//!      Qdrant never returns cross-tenant points.
//!   2. Optional `repo_id` filter further narrows to a single repository that
//!      must belong to the caller's tenant (validated against Postgres).

use axum::{Json, extract::State, response::IntoResponse};
use rb_query::{DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT, SearchOptions, semantic_search};
use rb_schemas::TenantId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Optional filters applied on top of the vector similarity ranking.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchFilters {
    /// Restrict results to a single repository UUID.
    pub repo_id: Option<Uuid>,
}

/// Body for `POST /v1/search`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchRequest {
    /// Natural-language query to embed and search.
    pub q: String,
    /// Maximum number of results to return (default 10, max 50).
    pub limit: Option<u32>,
    /// Optional result filters.
    pub filters: Option<SearchFilters>,
}

/// A single ranked result returned by `/v1/search`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResult {
    /// Fully-qualified name (e.g. `my_crate::module::my_fn`).
    pub fqn: String,
    /// Top-level crate name extracted from the FQN.
    pub crate_name: String,
    /// Repository UUID this symbol belongs to.
    pub repo_id: String,
    /// Cosine similarity score in `[0, 1]`.
    pub score: f32,
}

/// Response body for `POST /v1/search`.
#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

fn require_read_access(auth: AuthContext) -> Result<Uuid, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) if info.scopes.contains(&Scope::Read) => Ok(info.tenant_id),
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::McpJwt(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// Ollama embedding
// ---------------------------------------------------------------------------

#[allow(clippy::cast_possible_truncation)]
async fn embed_query(
    http: &reqwest::Client,
    ollama_url: &str,
    model: &str,
    query: &str,
) -> Result<Vec<f32>, AppError> {
    let url = format!("{}/api/embeddings", ollama_url.trim_end_matches('/'));
    let body = serde_json::json!({ "model": model, "prompt": query });

    let resp = http.post(&url).json(&body).send().await.map_err(|e| {
        tracing::warn!("Ollama request failed: {e}");
        AppError::ServiceUnavailable
    })?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!("Ollama returned HTTP {status}: {text}");
        return Err(AppError::ServiceUnavailable);
    }

    let json: serde_json::Value = resp.json().await.map_err(|e| {
        tracing::warn!("Ollama response parse error: {e}");
        AppError::ServiceUnavailable
    })?;

    let embedding = json
        .get("embedding")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            tracing::warn!("Ollama response missing 'embedding' array");
            AppError::ServiceUnavailable
        })?;

    embedding
        .iter()
        .map(|v| {
            v.as_f64()
                .map(|f| f as f32)
                .ok_or(AppError::ServiceUnavailable)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Semantic search across embedded code symbols within the caller's tenant.
///
/// Embeds `q` via Ollama, performs approximate nearest-neighbour search in
/// the Qdrant `rb_embeddings` collection filtered by `tenant_id`, and returns
/// ranked results with their fully-qualified names and crate context.
///
/// Returns 503 when either Qdrant (`RB_QDRANT_URL`) or Ollama (`RB_OLLAMA_URL`)
/// are not configured on this instance.
#[utoipa::path(
    post,
    path = "/v1/search",
    request_body = SearchRequest,
    responses(
        (status = 200, description = "Ranked search results", body = SearchResponse),
        (status = 400, description = "Invalid request (query empty or limit out of range)"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Email not verified or API key lacks read scope"),
        (status = 503, description = "Qdrant or Ollama not configured on this instance"),
    ),
    tag = "search"
)]
pub async fn search(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<SearchRequest>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id_uuid = require_read_access(auth)?;

    if req.q.trim().is_empty() {
        return Err(AppError::InvalidInput);
    }

    let qdrant = state
        .qdrant
        .as_deref()
        .ok_or(AppError::ServiceUnavailable)?;
    let ollama_url = state
        .config
        .ollama_url
        .as_deref()
        .ok_or(AppError::ServiceUnavailable)?;

    let limit = req
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);

    let repo_id_filter = req.filters.as_ref().and_then(|f| f.repo_id);

    // Validate repo ownership when a repo_id filter is supplied.
    if let Some(rid) = repo_id_filter {
        let owned: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM control.repos \
             WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
        )
        .bind(rid)
        .bind(tenant_id_uuid)
        .fetch_optional(&state.pool)
        .await?;
        owned.ok_or(AppError::NotFound)?;
    }

    let http = reqwest::Client::new();
    let vector = embed_query(&http, ollama_url, &state.config.embedding_model, &req.q).await?;

    let tenant_id = TenantId::from(tenant_id_uuid);
    let opts = SearchOptions {
        limit,
        repo_id: repo_id_filter,
    };
    let hits = semantic_search(qdrant, &tenant_id, &vector, opts).await?;

    let results: Vec<SearchResult> = hits
        .into_iter()
        .map(|h| {
            let crate_name = h.fqn.split("::").next().unwrap_or(&h.fqn).to_owned();
            SearchResult {
                fqn: h.fqn,
                crate_name,
                repo_id: h.repo_id,
                score: h.score,
            }
        })
        .collect();

    tracing::debug!(
        tenant_id = %tenant_id_uuid,
        query = %req.q,
        result_count = results.len(),
        "semantic search completed"
    );

    Ok(Json(SearchResponse { results }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session(tenant_id: Uuid) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        }
    }

    #[test]
    fn anonymous_rejected() {
        assert!(matches!(
            require_read_access(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn expired_session_rejected() {
        assert!(matches!(
            require_read_access(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn unverified_session_rejected() {
        let mut info = verified_session(Uuid::new_v4());
        info.email_verified = false;
        assert!(matches!(
            require_read_access(AuthContext::Session(info)),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn verified_session_accepted() {
        let tid = Uuid::new_v4();
        let result = require_read_access(AuthContext::Session(verified_session(tid)));
        assert_eq!(result.unwrap(), tid);
    }

    #[test]
    fn api_key_with_read_scope_accepted() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        assert_eq!(require_read_access(AuthContext::ApiKey(key)).unwrap(), tid);
    }

    #[test]
    fn api_key_without_read_scope_rejected() {
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Write],
        };
        assert!(matches!(
            require_read_access(AuthContext::ApiKey(key)),
            Err(AppError::InsufficientScope)
        ));
    }

    #[test]
    fn crate_name_extracted_from_fqn() {
        let fqn = "my_crate::module::MyStruct";
        let crate_name = fqn.split("::").next().unwrap_or(fqn).to_owned();
        assert_eq!(crate_name, "my_crate");
    }

    #[test]
    fn crate_name_for_bare_fqn() {
        let fqn = "bare_crate";
        let crate_name = fqn.split("::").next().unwrap_or(fqn).to_owned();
        assert_eq!(crate_name, "bare_crate");
    }

    #[test]
    fn limit_defaults_and_cap() {
        let applied = DEFAULT_SEARCH_LIMIT.clamp(1, MAX_SEARCH_LIMIT);
        assert_eq!(applied, DEFAULT_SEARCH_LIMIT);

        let over = 200_u32.clamp(1, MAX_SEARCH_LIMIT);
        assert_eq!(over, MAX_SEARCH_LIMIT);
    }
}
