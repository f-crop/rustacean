//! Hybrid retrieval: dense (Qdrant ANN) + sparse (Postgres FTS) fused via RRF.
//!
//! Entry point: [`hybrid_search`]. Mirrors the signature of [`semantic_search`] but
//! adds a [`PgPool`] and query-text for the sparse leg. Both legs are tenant-isolated:
//! - Dense: `TenantVectorStore::search` injects a `must` `tenant_id` filter (ADR-007 §13.2).
//! - Sparse: query runs against the per-tenant schema (`TenantCtx::qualify`) — physically
//!   isolated; no extra filter required (ADR-014 §10).
//!
//! The pure fusion function `rrf_fuse` is private; unit tests within this module
//! access it directly to verify the k=60 math without a live database or Qdrant instance.
//!
//! Rerank is intentionally **not** applied here — it is the caller's (control-api)
//! responsibility, keeping `rb-query` free of a dependency on `rb-rerank` (ADR-014 §2).

use std::collections::HashMap;

use rb_schemas::TenantId;
use rb_storage_qdrant::TenantVectorStore;
use rb_tenant::TenantCtx;
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    QueryError,
    vector::search::{SearchOptions, SemanticHit, semantic_search},
};

/// RRF smoothing constant (ADR-014 §3.3). Fixed for v1; k=60 is the standard baseline.
pub const RRF_K: f32 = 60.0;

/// Minimum fetch depth per leg: `N_fetch = max(limit, MIN_FETCH)`.
const MIN_FETCH: u32 = 50;

/// Options for [`hybrid_search`].
pub struct HybridSearchOptions {
    /// Maximum number of fused results to return (after fusion + truncation).
    pub limit: u32,
    /// When set, restricts both legs to a single repository.
    pub repo_id: Option<Uuid>,
}

/// A fused result from the hybrid retrieval path.
#[derive(Debug, Clone)]
pub struct HybridHit {
    /// Fully-qualified name of the matched code symbol.
    pub fqn: String,
    /// Repository UUID (as string, matching `SemanticHit` convention).
    pub repo_id: String,
    /// Relative path within the repo (from `code_symbols.source_path`).
    pub source_path: Option<String>,
    /// First line of the symbol (1-indexed, inclusive).
    pub line_start: Option<i32>,
    /// Last line of the symbol (1-indexed, inclusive).
    pub line_end: Option<i32>,
    /// RRF-fused score normalized to `[0, 1]`. Higher is more relevant.
    pub score: f32,
}

type SparseRow = (String, String, Option<String>, Option<i32>, Option<i32>);

/// Row type for `backfill_metadata` — `(fqn, source_path, line_start, line_end)`.
type BackfillRow = (String, Option<String>, Option<i32>, Option<i32>);

/// Raw row returned by the sparse FTS query.
#[derive(Debug, Clone)]
struct SparseHit {
    fqn: String,
    repo_id: String,
    source_path: Option<String>,
    line_start: Option<i32>,
    line_end: Option<i32>,
}

impl From<SparseRow> for SparseHit {
    fn from((fqn, repo_id, source_path, line_start, line_end): SparseRow) -> Self {
        Self {
            fqn,
            repo_id,
            source_path,
            line_start,
            line_end,
        }
    }
}

async fn sparse_search(
    pool: &PgPool,
    ctx: &TenantCtx,
    query_text: &str,
    limit: u32,
    repo_id: Option<Uuid>,
) -> Result<Vec<SparseHit>, QueryError> {
    // Schema is tenant-qualified — no additional tenant_id filter needed.
    let table = ctx.qualify("code_symbols");
    let rows: Vec<SparseRow> = match repo_id {
        Some(rid) => {
            sqlx::query_as(&format!(
                "SELECT fqn, repo_id::text, source_path, line_start, line_end \
                 FROM {table} \
                 WHERE fts @@ plainto_tsquery('simple', $1) \
                   AND repo_id = $2 \
                 ORDER BY ts_rank_cd(fts, plainto_tsquery('simple', $1)) DESC \
                 LIMIT $3",
            ))
            .bind(query_text)
            .bind(rid)
            .bind(i64::from(limit))
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as(&format!(
                "SELECT fqn, repo_id::text, source_path, line_start, line_end \
                 FROM {table} \
                 WHERE fts @@ plainto_tsquery('simple', $1) \
                 ORDER BY ts_rank_cd(fts, plainto_tsquery('simple', $1)) DESC \
                 LIMIT $2",
            ))
            .bind(query_text)
            .bind(i64::from(limit))
            .fetch_all(pool)
            .await?
        }
    };

    Ok(rows.into_iter().map(SparseHit::from).collect())
}

// ---------------------------------------------------------------------------
// Dense-metadata backfill
// ---------------------------------------------------------------------------
/// Fetch `source_path`/`line_start`/`line_end` from `code_symbols` for dense-only hits
/// absent from the sparse leg. Uses `fqn = ANY($1)` in the tenant-qualified schema.
async fn backfill_metadata(
    pool: &PgPool,
    ctx: &TenantCtx,
    fqns: &[String],
) -> Result<HashMap<String, (Option<String>, Option<i32>, Option<i32>)>, QueryError> {
    if fqns.is_empty() {
        return Ok(HashMap::new());
    }

    let table = ctx.qualify("code_symbols");
    // Collect as &str so sqlx encodes as a Postgres text[] parameter.
    let fqn_strs: Vec<&str> = fqns.iter().map(String::as_str).collect();
    let rows: Vec<BackfillRow> = sqlx::query_as(&format!(
        "SELECT fqn, source_path, line_start, line_end \
         FROM {table} \
         WHERE fqn = ANY($1)",
    ))
    .bind(&fqn_strs as &[&str])
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(fqn, sp, ls, le)| (fqn, (sp, ls, le)))
        .collect())
}

/// Normalize scores, backfill dense-only metadata from `code_symbols`, and assemble hits.
/// Shared post-fusion step for [`hybrid_search`] and [`hybrid_search_multi`].
async fn build_annotated_hits(
    pool: &PgPool,
    ctx: &TenantCtx,
    fused: Vec<(String, String, f32)>,
    sparse_hits: &[SparseHit],
) -> Result<Vec<HybridHit>, QueryError> {
    let max_score = fused.iter().map(|(_, _, s)| *s).fold(0.0f32, f32::max);
    let sparse_meta: HashMap<&str, &SparseHit> =
        sparse_hits.iter().map(|h| (h.fqn.as_str(), h)).collect();
    let missing: Vec<String> = fused
        .iter()
        .filter(|(fqn, _, _)| !sparse_meta.contains_key(fqn.as_str()))
        .map(|(fqn, _, _)| fqn.clone())
        .collect();
    let dense_meta = backfill_metadata(pool, ctx, &missing).await?;
    Ok(fused
        .into_iter()
        .map(|(fqn, repo_id, raw_score)| {
            let normalized = if max_score > 0.0 {
                raw_score / max_score
            } else {
                0.0
            };
            let (source_path, line_start, line_end) =
                if let Some(m) = sparse_meta.get(fqn.as_str()).copied() {
                    (m.source_path.clone(), m.line_start, m.line_end)
                } else {
                    let m = dense_meta.get(&fqn);
                    (
                        m.and_then(|(sp, _, _)| sp.clone()),
                        m.and_then(|(_, ls, _)| *ls),
                        m.and_then(|(_, _, le)| *le),
                    )
                };
            HybridHit {
                source_path,
                line_start,
                line_end,
                fqn,
                repo_id,
                score: normalized,
            }
        })
        // Drop orphan embeddings whose fqn has no row in code_symbols: an empty
        // source_path means the LLM would reference a citation the user cannot open.
        .filter(|h| {
            h.source_path
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        })
        .collect())
}

// ---------------------------------------------------------------------------
// RRF fusion
// ---------------------------------------------------------------------------

/// Fuse two ranked lists via Reciprocal Rank Fusion (RRF, k=60).
///
/// Formula per hit `d`: `RRF(d) = Σ_legs 1 / (k + rank_leg(d))` where rank is 1-indexed.
/// Result is truncated to `limit` and returned in descending score order.
///
/// Returns `Vec<(fqn, repo_id, raw_rrf_score)>` — callers normalize the score.
fn rrf_fuse(
    dense: &[SemanticHit],
    sparse: &[SparseHit],
    k: f32,
    limit: usize,
) -> Vec<(String, String, f32)> {
    let mut scores: HashMap<&str, f32> = HashMap::new();

    for (rank, hit) in dense.iter().enumerate() {
        // rank is at most MIN_FETCH (50) — cast is safe in practice.
        #[allow(clippy::cast_precision_loss)]
        let contrib = 1.0 / (k + (rank + 1) as f32);
        *scores.entry(hit.fqn.as_str()).or_insert(0.0) += contrib;
    }
    for (rank, hit) in sparse.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let contrib = 1.0 / (k + (rank + 1) as f32);
        *scores.entry(hit.fqn.as_str()).or_insert(0.0) += contrib;
    }

    // Build fqn → repo_id lookup (dense wins on tie; both legs agree in practice).
    let mut fqn_repo: HashMap<&str, &str> = HashMap::new();
    for h in dense {
        fqn_repo.entry(h.fqn.as_str()).or_insert(h.repo_id.as_str());
    }
    for h in sparse {
        fqn_repo.entry(h.fqn.as_str()).or_insert(h.repo_id.as_str());
    }

    let mut ranked: Vec<(&str, f32)> = scores.into_iter().collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    ranked.truncate(limit);

    ranked
        .into_iter()
        .map(|(fqn, rrf_score)| {
            let repo_id = fqn_repo.get(fqn).copied().unwrap_or("").to_owned();
            (fqn.to_owned(), repo_id, rrf_score)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Hybrid semantic + full-text search within a single tenant's code corpus.
///
/// Runs the dense leg (`semantic_search` via Qdrant) and the sparse leg (Postgres
/// `ts_rank_cd`) to depth `N_fetch = max(limit, 50)`, fuses results
/// via RRF (k=60), and returns the top `limit` hits in descending score order.
///
/// Both legs enforce tenant isolation independently (see module-level docs).
///
/// # Errors
///
/// Returns [`QueryError::Database`] on Postgres failure or [`QueryError::Qdrant`]
/// on Qdrant failure.
pub async fn hybrid_search(
    pool: &PgPool,
    store: &TenantVectorStore,
    tenant_id: &TenantId,
    vector: &[f32],
    query_text: &str,
    opts: HybridSearchOptions,
) -> Result<Vec<HybridHit>, QueryError> {
    let n_fetch = opts.limit.max(MIN_FETCH);
    let ctx = TenantCtx::new(*tenant_id);

    // Run both legs concurrently — neither leg depends on the other's result.
    let (dense_hits, sparse_hits) = tokio::try_join!(
        semantic_search(
            store,
            tenant_id,
            vector,
            SearchOptions {
                limit: n_fetch,
                repo_id: opts.repo_id,
            },
        ),
        sparse_search(pool, &ctx, query_text, n_fetch, opts.repo_id)
    )?;

    // Fuse via RRF, then normalize scores, backfill dense metadata, and build hits.
    #[allow(clippy::cast_possible_truncation)]
    let fused = rrf_fuse(&dense_hits, &sparse_hits, RRF_K, opts.limit as usize);
    build_annotated_hits(pool, &ctx, fused, &sparse_hits).await
}

// ---------------------------------------------------------------------------
// Multi-query entry point (AC2)
// ---------------------------------------------------------------------------

/// Multi-query hybrid search: run one `hybrid_search` leg per query variant, then
/// fuse **all** dense + sparse hits from all variants in a single RRF pass.
///
/// `query_variants` is a slice of `(embedding_vector, query_text)` pairs — produced
/// by `rb_query::rewrite::expand_query` + the embedding call.  When the slice has
/// exactly one element, this is byte-equivalent to calling [`hybrid_search`] directly
/// (AC7).
///
/// Tenant isolation is unchanged: each leg inherits the `tenant_id` filter from
/// [`hybrid_search`], so variants cannot bleed across tenants (AC6).
///
/// # Errors
///
/// Returns [`QueryError::Database`] or [`QueryError::Qdrant`] on leg failure.
pub async fn hybrid_search_multi(
    pool: &PgPool,
    store: &TenantVectorStore,
    tenant_id: &TenantId,
    query_variants: &[(Vec<f32>, String)],
    opts: HybridSearchOptions,
) -> Result<Vec<HybridHit>, QueryError> {
    if query_variants.is_empty() {
        return Ok(vec![]);
    }

    let n_fetch = opts.limit.max(MIN_FETCH);
    let ctx = TenantCtx::new(*tenant_id);

    // Collect hits from every variant. All dense + sparse hits feed into one fusion.
    let mut all_dense: Vec<SemanticHit> = Vec::new();
    let mut all_sparse: Vec<SparseHit> = Vec::new();

    for (vector, query_text) in query_variants {
        let dense = semantic_search(
            store,
            tenant_id,
            vector,
            SearchOptions {
                limit: n_fetch,
                repo_id: opts.repo_id,
            },
        )
        .await?;
        let sparse = sparse_search(pool, &ctx, query_text, n_fetch, opts.repo_id).await?;
        all_dense.extend(dense);
        all_sparse.extend(sparse);
    }

    // Single flat RRF fusion across all variant hits (not nested).
    #[allow(clippy::cast_possible_truncation)]
    let fused = rrf_fuse(&all_dense, &all_sparse, RRF_K, opts.limit as usize);
    build_annotated_hits(pool, &ctx, fused, &all_sparse).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "hybrid_tests.rs"]
mod tests;
