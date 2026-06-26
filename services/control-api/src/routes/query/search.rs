//! `POST /v1/search` — semantic code search (REQ-DP-01).
//!
//! When `RB_HYBRID_SEARCH_ENABLED=false` (default): embeds the query via Ollama,
//! searches Qdrant with a `tenant_id` must-filter, and returns ranked code symbols.
//! Response shape: `{"results":[...]}` — byte-identical to the pre-Wave-10 path.
//!
//! When `RB_HYBRID_SEARCH_ENABLED=true`: runs dense + sparse legs via
//! `rb_query::hybrid_search`, fuses via RRF k=60, sources `commit_sha` from
//! `control.ingestion_runs`, and populates `citations` (`CitationV1` envelope).
//!
//! Multi-tenancy is enforced at two layers:
//!   1. [`TenantVectorStore::search`] injects a `must` `tenant_id` filter so
//!      Qdrant never returns cross-tenant points.
//!   2. Optional `repo_id` filter further narrows to a single repository that
//!      must belong to the caller's tenant (validated against Postgres).

use axum::{Json, extract::State, response::IntoResponse};
use rb_query::{
    DEFAULT_SEARCH_LIMIT, HybridSearchOptions, MAX_SEARCH_LIMIT, MultiQueryConfig, SearchOptions,
    expand_query, hybrid_search_multi, resolve_n, semantic_search,
};
use rb_schemas::{CitationV1, LineRange, SourceKind, TenantId};
use serde::{Deserialize, Serialize};
use sqlx::Row as _;
use std::collections::HashMap;
use std::time::Instant;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    embed::normalize_query,
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
///
/// `results` is always present (backward compat).
/// `citations` is populated only when `RB_HYBRID_SEARCH_ENABLED=true`; absent otherwise
/// (skipped in serialization when empty) so flag-off response is byte-identical to pre-S2.
#[derive(Debug, Serialize, ToSchema)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<CitationV1>,
}

pub(crate) use super::cost_ceilings::{clamp_rerank_candidates, llm_budget_allows};

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
    let prompt = normalize_query(query);
    let body = serde_json::json!({ "model": model, "prompt": prompt });

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
// Commit-SHA sourcing (hybrid path only)
// ---------------------------------------------------------------------------

/// Fetch the latest non-null `commit_sha` from `control.ingestion_runs` for each
/// distinct `repo_id` in `repo_ids`. Returns a map `repo_id → commit_sha`.
///
/// Repos with no succeeded run (or all runs have NULL `commit_sha`) are mapped to
/// `"unknown"` per ADR-014 §5 ("`commit_sha` must not be `Option` or empty").
async fn fetch_commit_shas(
    pool: &sqlx::PgPool,
    repo_ids: &[Uuid],
) -> Result<HashMap<Uuid, String>, AppError> {
    if repo_ids.is_empty() {
        return Ok(HashMap::new());
    }

    // Latest non-null commit_sha per repo (most recent started run first).
    let rows = sqlx::query(
        "SELECT DISTINCT ON (repo_id) repo_id, commit_sha \
         FROM control.ingestion_runs \
         WHERE repo_id = ANY($1) \
           AND commit_sha IS NOT NULL \
         ORDER BY repo_id, started_at DESC NULLS LAST",
    )
    .bind(repo_ids)
    .fetch_all(pool)
    .await?;

    let mut map: HashMap<Uuid, String> = rows
        .into_iter()
        .map(|r| {
            let repo_id: Uuid = r.get("repo_id");
            let sha: String = r.get("commit_sha");
            (repo_id, sha)
        })
        .collect();

    // Fill "unknown" for repos with no ingestion run yet.
    for rid in repo_ids {
        map.entry(*rid).or_insert_with(|| "unknown".to_owned());
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Per-tenant query settings (Wave 10 S5)
// ---------------------------------------------------------------------------

/// Fetch per-tenant multi-query settings, falling back to the global config default.
pub(crate) async fn fetch_tenant_query_settings(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    global_n: u32,
) -> Result<MultiQueryConfig, AppError> {
    let row: Option<(i16, bool, i32)> = sqlx::query_as(
        "SELECT multi_query_n, multi_query_force_off, llm_token_budget \
         FROM control.tenant_query_settings \
         WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_optional(pool)
    .await?;

    let (tenant_n, force_off, budget) = row.map_or((global_n, false, 0u32), |(n, fo, b)| {
        (n.unsigned_abs().into(), fo, b.unsigned_abs())
    });

    Ok(MultiQueryConfig {
        n: resolve_n(tenant_n, force_off),
        force_off,
        token_budget: budget,
    })
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
/// When `RB_HYBRID_SEARCH_ENABLED=true`, also runs Postgres FTS and fuses
/// results via RRF k=60, returning `CitationV1` envelopes in `citations`.
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
#[allow(clippy::too_many_lines)]
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

    // AC5: guard LLM calls before they reach any rewriter/reranker.
    // ceiling=0 (default) → zero outbound LLM cost for all tenants.
    let _llm_allowed =
        llm_budget_allows(state.config.llm_token_ceiling_per_tenant, 0, tenant_id_uuid);

    if state.config.hybrid_search_enabled {
        // --- Hybrid path (flag on) ---
        // Resolve per-tenant multi-query config (S5). Default n=1 means no rewrite.
        let mq_config =
            fetch_tenant_query_settings(&state.pool, tenant_id_uuid, state.config.multi_query_n)
                .await?;

        // Expand the query into variants (returns [original] when n=1 or disabled).
        let query_texts = expand_query(
            &mq_config,
            &http,
            ollama_url,
            &state.config.embedding_model,
            &req.q,
        )
        .await;

        // Embed each query variant (reuse already-computed vector for the original).
        let mut query_variants: Vec<(Vec<f32>, String)> = Vec::with_capacity(query_texts.len());
        for qt in &query_texts {
            let v = if qt == &req.q {
                vector.clone()
            } else {
                embed_query(&http, ollama_url, &state.config.embedding_model, qt).await?
            };
            query_variants.push((v, qt.clone()));
        }

        let t0 = Instant::now();
        let hits = hybrid_search_multi(
            &state.pool,
            qdrant,
            &tenant_id,
            &query_variants,
            HybridSearchOptions {
                limit,
                repo_id: repo_id_filter,
            },
        )
        .await
        .map_err(|e| {
            tracing::warn!("hybrid_search_multi failed: {e}");
            AppError::ServiceUnavailable
        })?;
        #[allow(clippy::cast_precision_loss)]
        let elapsed_ms = t0.elapsed().as_micros() as f64 / 1000.0;

        // AC1: emit duration histogram and candidate counter for the hybrid leg.
        metrics::histogram!("retrieval_request_duration_ms", "mode" => "hybrid").record(elapsed_ms);
        metrics::counter!("retrieval_candidates_total", "mode" => "hybrid")
            .increment(hits.len() as u64);

        // AC3: clamp rerank candidate set before any future cross-encoder call.
        let hits = clamp_rerank_candidates(hits, state.config.rerank_candidate_cap, tenant_id_uuid);

        // AC3 / AC4: optional cross-encoder rerank stage (flag-gated, S3).
        let (hits, citation_source_kind) = if let Some(reranker) = state.reranker.as_deref() {
            let candidates: Vec<rb_rerank::RerankCandidate> = hits
                .iter()
                .enumerate()
                .map(|(i, h)| rb_rerank::RerankCandidate {
                    original_idx: i,
                    text: h.fqn.clone(),
                    original_score: h.score,
                })
                .collect();
            match reranker.rerank(&req.q, candidates).await {
                Ok(ranked) => {
                    let reranked: Vec<rb_query::HybridHit> = ranked
                        .iter()
                        .map(|r| hits[r.original_idx].clone())
                        .collect();
                    metrics::counter!("retrieval_rerank_applied_total").increment(1);
                    (reranked, SourceKind::Rerank)
                }
                Err(e) => {
                    tracing::warn!(tenant_id = %tenant_id_uuid, "reranker error, using RRF order: {e}");
                    (hits, SourceKind::Hybrid)
                }
            }
        } else {
            (hits, SourceKind::Hybrid)
        };

        tracing::debug!(
            tenant_id = %tenant_id_uuid,
            query = %req.q,
            result_count = hits.len(),
            elapsed_ms,
            "hybrid search completed"
        );

        // Collect distinct repo_ids for commit_sha lookup.
        let repo_ids: Vec<Uuid> = {
            let mut seen = std::collections::HashSet::new();
            hits.iter()
                .filter_map(|h| h.repo_id.parse::<Uuid>().ok())
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let commit_shas = fetch_commit_shas(&state.pool, &repo_ids).await?;

        let results: Vec<SearchResult> = hits
            .iter()
            .map(|h| {
                let crate_name = h.fqn.split("::").next().unwrap_or(&h.fqn).to_owned();
                SearchResult {
                    fqn: h.fqn.clone(),
                    crate_name,
                    repo_id: h.repo_id.clone(),
                    score: h.score,
                }
            })
            .collect();

        let citations: Vec<CitationV1> = hits
            .into_iter()
            .map(|h| {
                let repo_uuid = h.repo_id.parse::<Uuid>().unwrap_or(Uuid::nil());
                let commit_sha = commit_shas
                    .get(&repo_uuid)
                    .cloned()
                    .unwrap_or_else(|| "unknown".to_owned());
                CitationV1 {
                    version: CitationV1::VERSION.to_owned(),
                    repo_id: repo_uuid,
                    file_path: h.source_path.unwrap_or_default(),
                    line_range: LineRange {
                        start: h.line_start.unwrap_or(0),
                        end: h.line_end.unwrap_or(0),
                    },
                    commit_sha,
                    score: h.score,
                    source_kind: citation_source_kind,
                }
            })
            .collect();

        Ok(Json(SearchResponse { results, citations }))
    } else {
        // --- Dense-only path (flag off) — byte-identical to pre-S2 ---
        let t0 = Instant::now();
        let opts = SearchOptions {
            limit,
            repo_id: repo_id_filter,
        };
        let hits = semantic_search(qdrant, &tenant_id, &vector, opts).await?;
        #[allow(clippy::cast_precision_loss)]
        let elapsed_ms = t0.elapsed().as_micros() as f64 / 1000.0;

        // AC1: emit duration histogram and candidate counter for the dense leg.
        metrics::histogram!("retrieval_request_duration_ms", "mode" => "dense").record(elapsed_ms);
        metrics::counter!("retrieval_candidates_total", "mode" => "dense")
            .increment(hits.len() as u64);

        tracing::debug!(
            tenant_id = %tenant_id_uuid,
            query = %req.q,
            result_count = hits.len(),
            elapsed_ms,
            "semantic search completed"
        );

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

        Ok(Json(SearchResponse {
            results,
            citations: vec![],
        }))
    }
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
