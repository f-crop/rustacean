//! Semantic (vector) search via Qdrant, enriched with Postgres metadata.
//!
//! Bridges `rb-storage-qdrant` (ANN search) with the tenant's `code_symbols`
//! Postgres table to return fully-resolved symbol hits.
//!
//! Every Qdrant query applies a mandatory `tenant_id` must-filter for tenant
//! isolation (ADR-008 §3.1 security constraint).

use std::collections::HashMap;

use rb_storage_qdrant::{DEFAULT_SCORE_FLOOR, SearchOptions, search as qdrant_search};
use rb_tenant::TenantCtx;
use sqlx::PgPool;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A single result from semantic vector search, enriched with Postgres metadata.
#[derive(Debug, Clone)]
pub struct SemanticHit {
    pub fqn: String,
    pub repo_id: Uuid,
    pub kind: String,
    pub source_path: Option<String>,
    pub score: f32,
}

/// Error type for semantic search operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SemanticSearchError {
    #[error("Qdrant search failed: {0}")]
    Qdrant(#[from] rb_storage_qdrant::QdrantError),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Perform vector search against Qdrant, then enrich hits with Postgres metadata.
///
/// `query_vector` is a pre-computed embedding for the search query.
/// The mandatory `tenant_id` filter is always applied in Qdrant — this is the
/// structural guarantee preventing cross-tenant access.
///
/// When `kind_filter` is `Some`, the function over-fetches from Qdrant (×3, capped
/// at 300) to compensate for post-hoc kind filtering after Postgres enrichment.
///
/// Returns `(hits, next_offset)` where `next_offset` is the opaque Qdrant offset
/// for the next page, or `None` when results are exhausted.
///
/// # Errors
///
/// Returns [`SemanticSearchError`] on Qdrant or Postgres failure.
#[allow(clippy::too_many_arguments)]
pub async fn search_by_vector(
    http: &reqwest::Client,
    qdrant_url: &str,
    pool: &PgPool,
    tenant_ctx: &TenantCtx,
    tenant_id: Uuid,
    query_vector: &[f32],
    repo_id_filter: Option<Uuid>,
    kind_filter: Option<&str>,
    limit: u32,
    offset: u32,
) -> Result<(Vec<SemanticHit>, Option<u32>), SemanticSearchError> {
    let tenant_id_str = tenant_id.to_string();
    let repo_id_str = repo_id_filter.map(|id| id.to_string());

    // Over-fetch when kind filtering is active to compensate for post-hoc removal
    // of non-matching symbols.
    let qdrant_limit = if kind_filter.is_some() {
        limit.saturating_mul(3).min(300)
    } else {
        limit
    };

    let opts = SearchOptions {
        qdrant_url,
        tenant_id: &tenant_id_str,
        query_vector,
        repo_id_filter: repo_id_str.as_deref(),
        score_threshold: DEFAULT_SCORE_FLOOR,
        limit: qdrant_limit,
        offset,
    };

    let qdrant_results = qdrant_search(http, opts).await?;

    if qdrant_results.hits.is_empty() {
        return Ok((Vec::new(), qdrant_results.next_offset));
    }

    // Parse repo_ids from Qdrant hit payload strings; skip malformed entries.
    let qdrant_triples: Vec<(Uuid, String, f32)> = qdrant_results
        .hits
        .iter()
        .filter_map(|h| {
            h.repo_id
                .parse::<Uuid>()
                .ok()
                .map(|rid| (rid, h.fqn.clone(), h.score))
        })
        .collect();

    if qdrant_triples.is_empty() {
        return Ok((Vec::new(), qdrant_results.next_offset));
    }

    // Batch-fetch code_symbols rows for kind + source_path enrichment.
    let repo_ids: Vec<Uuid> = qdrant_triples.iter().map(|(rid, _, _)| *rid).collect();
    let fqns: Vec<String> = qdrant_triples
        .iter()
        .map(|(_, fqn, _)| fqn.clone())
        .collect();
    let table = tenant_ctx.qualify("code_symbols");

    // `WHERE repo_id = ANY($1) AND fqn = ANY($2)` may produce false-positive
    // pairs if the same FQN exists in different repos; these are filtered in
    // the HashMap join below.
    let pg_rows: Vec<(String, String, Option<String>, Uuid)> = sqlx::query_as(&format!(
        "SELECT fqn, kind, source_path, repo_id \
         FROM {table} \
         WHERE repo_id = ANY($1) AND fqn = ANY($2)"
    ))
    .bind(&repo_ids[..])
    .bind(&fqns[..])
    .fetch_all(pool)
    .await?;

    // Build lookup: (repo_id, fqn) → (kind, source_path)
    let mut pg_map: HashMap<(Uuid, String), (String, Option<String>)> = HashMap::new();
    for (fqn, kind, source_path, repo_id) in pg_rows {
        pg_map.insert((repo_id, fqn), (kind, source_path));
    }

    // Join Qdrant scores with Postgres metadata; apply optional kind filter.
    let hits: Vec<SemanticHit> = qdrant_triples
        .into_iter()
        .filter_map(|(repo_id, fqn, score)| {
            let (kind, source_path) = pg_map.remove(&(repo_id, fqn.clone()))?;
            if let Some(kf) = kind_filter {
                if !kind.eq_ignore_ascii_case(kf) {
                    return None;
                }
            }
            Some(SemanticHit {
                fqn,
                repo_id,
                kind,
                source_path,
                score,
            })
        })
        .take(limit as usize)
        .collect();

    // Qdrant already returns results in descending score order; take(limit)
    // preserves that ordering.

    Ok((hits, qdrant_results.next_offset))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_hit_fields_accessible() {
        let hit = SemanticHit {
            fqn: "crate::mod::Fn".to_owned(),
            repo_id: Uuid::new_v4(),
            kind: "FN".to_owned(),
            source_path: Some("src/lib.rs".to_owned()),
            score: 0.85,
        };
        assert_eq!(hit.kind, "FN");
        assert!(hit.score > 0.0);
    }

    #[test]
    fn semantic_hit_without_source_path() {
        let hit = SemanticHit {
            fqn: "crate::Trait".to_owned(),
            repo_id: Uuid::new_v4(),
            kind: "TRAIT".to_owned(),
            source_path: None,
            score: 0.42,
        };
        assert!(hit.source_path.is_none());
    }

    #[test]
    fn semantic_search_error_from_sqlx() {
        let err = SemanticSearchError::Database(sqlx::Error::RowNotFound);
        assert!(err.to_string().contains("database error"));
    }
}
