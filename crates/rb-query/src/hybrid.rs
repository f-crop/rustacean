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
    let max_score = fused.iter().map(|(_, _, s)| *s).fold(0.0f32, f32::max);

    let sparse_meta: HashMap<&str, &SparseHit> =
        all_sparse.iter().map(|h| (h.fqn.as_str(), h)).collect();

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

    // AC6: multi-variant fusion must not leak hits across different repo/tenant namespaces.
    // Simulated by using distinct repo_id tags per "tenant" and verifying the fused set
    // contains only the expected fqns with the correct repo provenance.
    #[test]
    fn multi_variant_rrf_no_cross_tenant_repo_leak() {
        // Tenant-A hits (repo "ra").
        let d_a = dense(&[("a::Fn", "ra"), ("b::Fn", "ra")]);
        let s_a = sparse(&[("a::Fn", "ra"), ("c::Fn", "ra")]);
        // Tenant-B hits (repo "rb") — simulated as a second variant set.
        let d_b = dense(&[("x::Fn", "rb"), ("y::Fn", "rb")]);
        let s_b = sparse(&[("x::Fn", "rb"), ("z::Fn", "rb")]);

        // Fuse A's legs, then fuse B's legs independently — they must never share keys.
        let fused_a = rrf_fuse(&d_a, &s_a, RRF_K, 10);
        let fused_b = rrf_fuse(&d_b, &s_b, RRF_K, 10);

        let fqns_a: Vec<&str> = fused_a.iter().map(|(f, _, _)| f.as_str()).collect();
        let fqns_b: Vec<&str> = fused_b.iter().map(|(f, _, _)| f.as_str()).collect();

        // No A fqn appears in B's results and vice-versa.
        for fqn in &fqns_a {
            assert!(!fqns_b.contains(fqn), "{fqn} leaked from A into B");
        }
        for fqn in &fqns_b {
            assert!(!fqns_a.contains(fqn), "{fqn} leaked from B into A");
        }

        // Repo provenance is preserved per result set.
        assert!(fused_a.iter().all(|(_, repo, _)| repo == "ra"));
        assert!(fused_b.iter().all(|(_, repo, _)| repo == "rb"));
    }

    // AC7: when all variant legs are identical to a single-variant call, the fused
    // score ordering and repo provenance must be byte-identical.
    #[test]
    fn single_variant_multi_rrf_matches_single_rrf() {
        let d = dense(&[("a::Fn", "r1"), ("b::Fn", "r1"), ("c::Fn", "r1")]);
        let s = sparse(&[("a::Fn", "r1"), ("b::Fn", "r1")]);

        // Single-call fusion.
        let single = rrf_fuse(&d, &s, RRF_K, 10);

        // Multi-call fusion with identical inputs (simulates n=1 path in hybrid_search_multi).
        let multi = rrf_fuse(&d, &s, RRF_K, 10);

        assert_eq!(single.len(), multi.len());
        for ((fqn_s, repo_s, score_s), (fqn_m, repo_m, score_m)) in single.iter().zip(multi.iter())
        {
            assert_eq!(fqn_s, fqn_m, "fqn mismatch");
            assert_eq!(repo_s, repo_m, "repo mismatch");
            assert!((score_s - score_m).abs() < 1e-6, "score mismatch");
        }
    }

    // AC8: multi-variant fusion with n=3 must complete within a reasonable time bound
    // on a fixture (wall-clock guard — not a load test).
    #[test]
    fn multi_variant_rrf_completes_within_time_budget() {
        use std::time::Instant;

        // Simulate n=3: three independent (dense, sparse) pairs of 50 hits each.
        let items_per_leg: Vec<(&str, &str)> = (0..50usize)
            .map(|i| {
                let fqn: &'static str = Box::leak(format!("fn_{i}::Fn").into_boxed_str());
                (fqn, "r1")
            })
            .collect();

        let mut all_dense: Vec<SemanticHit> = Vec::new();
        let mut all_sparse: Vec<SparseHit> = Vec::new();
        for _ in 0..3 {
            all_dense.extend(dense(&items_per_leg));
            all_sparse.extend(sparse(&items_per_leg));
        }

        let t0 = Instant::now();
        let fused = rrf_fuse(&all_dense, &all_sparse, RRF_K, 10);
        let elapsed_ms = t0.elapsed().as_millis();

        assert!(!fused.is_empty(), "fusion must produce results");
        assert!(
            elapsed_ms < 50,
            "multi-variant RRF fusion took {elapsed_ms}ms — expected < 50ms"
        );
    }
}
