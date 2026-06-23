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

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// RRF smoothing constant (ADR-014 §3.3). Fixed for v1; k=60 is the standard baseline.
pub const RRF_K: f32 = 60.0;

/// Minimum fetch depth per leg: `N_fetch = max(limit, MIN_FETCH)`.
const MIN_FETCH: u32 = 50;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

type SparseRow = (String, String, Option<String>, Option<i32>, Option<i32>);

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

// ---------------------------------------------------------------------------
// Sparse leg
// ---------------------------------------------------------------------------

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

    // Run both legs sequentially (tracer slice; S7 adds parallelism + budget guard).
    let dense_hits = semantic_search(
        store,
        tenant_id,
        vector,
        SearchOptions {
            limit: n_fetch,
            repo_id: opts.repo_id,
        },
    )
    .await?;
    let sparse_hits = sparse_search(pool, &ctx, query_text, n_fetch, opts.repo_id).await?;

    // Fuse via RRF, then normalize scores to [0, 1].
    #[allow(clippy::cast_possible_truncation)]
    let fused = rrf_fuse(&dense_hits, &sparse_hits, RRF_K, opts.limit as usize);
    let max_score = fused.iter().map(|(_, _, s)| *s).fold(0.0f32, f32::max);

    // Build metadata lookup from sparse hits (carries source_path, line_start/end).
    let sparse_meta: HashMap<&str, &SparseHit> =
        sparse_hits.iter().map(|h| (h.fqn.as_str(), h)).collect();

    let results = fused
        .into_iter()
        .map(|(fqn, repo_id, raw_score)| {
            let normalized = if max_score > 0.0 {
                raw_score / max_score
            } else {
                0.0
            };
            let meta = sparse_meta.get(fqn.as_str()).copied();
            HybridHit {
                source_path: meta.and_then(|m| m.source_path.clone()),
                line_start: meta.and_then(|m| m.line_start),
                line_end: meta.and_then(|m| m.line_end),
                fqn,
                repo_id,
                score: normalized,
            }
        })
        .collect();

    Ok(results)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn dense(items: &[(&str, &str)]) -> Vec<SemanticHit> {
        items
            .iter()
            .map(|(fqn, repo)| SemanticHit {
                fqn: (*fqn).to_owned(),
                repo_id: (*repo).to_owned(),
                score: 0.9,
            })
            .collect()
    }

    fn sparse(items: &[(&str, &str)]) -> Vec<SparseHit> {
        items
            .iter()
            .map(|(fqn, repo)| SparseHit {
                fqn: (*fqn).to_owned(),
                repo_id: (*repo).to_owned(),
                source_path: Some("src/lib.rs".to_owned()),
                line_start: Some(1),
                line_end: Some(10),
            })
            .collect()
    }

    // AC2(a): only dense hits — sparse is empty.
    #[test]
    fn rrf_only_dense_hits() {
        let d = dense(&[("a::Fn", "r1"), ("b::Fn", "r1"), ("c::Fn", "r1")]);
        let s: Vec<SparseHit> = vec![];
        let fused = rrf_fuse(&d, &s, RRF_K, 10);

        assert_eq!(fused.len(), 3);
        assert_eq!(fused[0].0, "a::Fn");
        assert_eq!(fused[1].0, "b::Fn");
        assert_eq!(fused[2].0, "c::Fn");
        assert!(fused.iter().all(|(_, _, s)| *s > 0.0));
    }

    // AC2(b): only sparse hits — dense is empty.
    #[test]
    fn rrf_only_sparse_hits() {
        let d: Vec<SemanticHit> = vec![];
        let s = sparse(&[("x::Struct", "r2"), ("y::Struct", "r2")]);
        let fused = rrf_fuse(&d, &s, RRF_K, 10);

        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].0, "x::Struct");
        assert!(fused[0].2 > fused[1].2);
    }

    // AC2(c): overlapping ranks — hit in both legs scores higher.
    #[test]
    fn rrf_overlapping_ranks_scores_higher() {
        let d = dense(&[("shared", "r1"), ("dense_only", "r1")]);
        let s = sparse(&[("shared", "r1"), ("sparse_only", "r1")]);
        let fused = rrf_fuse(&d, &s, RRF_K, 10);

        let score_shared = fused.iter().find(|(f, _, _)| f == "shared").unwrap().2;
        let score_dense = fused.iter().find(|(f, _, _)| f == "dense_only").unwrap().2;
        let score_sparse = fused.iter().find(|(f, _, _)| f == "sparse_only").unwrap().2;

        assert!(score_shared > score_dense);
        assert!(score_shared > score_sparse);
    }

    // AC2(d): k=60 math is correct.
    #[test]
    fn rrf_k60_math_correct() {
        let d = dense(&[("only", "r1")]);
        let s: Vec<SparseHit> = vec![];
        let fused = rrf_fuse(&d, &s, RRF_K, 10);

        assert_eq!(fused.len(), 1);
        let expected = 1.0_f32 / (60.0 + 1.0);
        assert!(
            (fused[0].2 - expected).abs() < 1e-6,
            "expected {expected}, got {}",
            fused[0].2
        );
    }

    #[test]
    fn rrf_truncates_to_limit() {
        let items: Vec<(&str, &str)> = (0..20usize)
            .map(|i| {
                let fqn: &'static str = Box::leak(format!("fn_{i}::Fn").into_boxed_str());
                (fqn, "r1")
            })
            .collect();
        let d = dense(&items);
        let s: Vec<SparseHit> = vec![];
        let fused = rrf_fuse(&d, &s, RRF_K, 5);
        assert_eq!(fused.len(), 5);
    }

    #[test]
    fn rrf_empty_both_legs_returns_empty() {
        let d: Vec<SemanticHit> = vec![];
        let s: Vec<SparseHit> = vec![];
        let fused = rrf_fuse(&d, &s, RRF_K, 10);
        assert!(fused.is_empty());
    }

    #[test]
    fn rrf_fuse_repo_id_from_dense_on_tie() {
        let d = dense(&[("shared", "dense-repo")]);
        let s = sparse(&[("shared", "sparse-repo")]);
        let fused = rrf_fuse(&d, &s, RRF_K, 10);
        let (_, repo, _) = &fused[0];
        assert_eq!(repo, "dense-repo");
    }
}
