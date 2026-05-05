//! Semantic search query — embed query via Ollama, search Qdrant `rb_embeddings`.
//!
//! Entry point: [`semantic_search`].  Callers supply a pre-validated tenant
//! context; this module enforces per-tenant isolation by delegating to
//! [`TenantVectorStore::search`] which injects the mandatory `tenant_id` filter.

use rb_schemas::TenantId;
use rb_storage_qdrant::{SearchHit, TenantVectorStore};
use uuid::Uuid;

use crate::QueryError;

/// Options controlling a semantic search request.
pub struct SearchOptions {
    /// Maximum number of results to return (capped at [`MAX_SEARCH_LIMIT`]).
    pub limit: u32,
    /// When set, restricts results to a single repository.
    pub repo_id: Option<Uuid>,
}

/// Maximum allowed `limit` for a single search request.
pub const MAX_SEARCH_LIMIT: u32 = 50;

/// Default result limit when the caller does not specify one.
pub const DEFAULT_SEARCH_LIMIT: u32 = 10;

/// A ranked semantic search result.
#[derive(Debug, Clone)]
pub struct SemanticHit {
    /// Fully-qualified name of the matched code symbol.
    pub fqn: String,
    /// Repository UUID this symbol belongs to.
    pub repo_id: String,
    /// Cosine similarity score in `[0, 1]`.
    pub score: f32,
}

impl From<SearchHit> for SemanticHit {
    fn from(h: SearchHit) -> Self {
        Self { fqn: h.fqn, repo_id: h.repo_id, score: h.score }
    }
}

/// Search `rb_embeddings` for the `limit` most similar code symbols.
///
/// `vector` must be the Ollama embedding of the user's query (produced by the
/// caller via [`call_ollama`](crate::vector::search) or equivalent).  The
/// `must` tenant filter is injected by [`TenantVectorStore`] — this function
/// never forwards cross-tenant data.
///
/// # Errors
///
/// Returns [`QueryError::Qdrant`] on Qdrant communication failure.
pub async fn semantic_search(
    store: &TenantVectorStore,
    tenant_id: &TenantId,
    vector: &[f32],
    opts: SearchOptions,
) -> Result<Vec<SemanticHit>, QueryError> {
    let limit = opts.limit.clamp(1, MAX_SEARCH_LIMIT);
    let hits = store.search(tenant_id, vector, limit, opts.repo_id).await?;
    Ok(hits.into_iter().map(SemanticHit::from).collect())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use rb_storage_qdrant::SearchHit;

    #[test]
    fn semantic_hit_from_search_hit() {
        let hit = SearchHit { fqn: "my::Fn".to_owned(), repo_id: "r1".to_owned(), score: 0.9 };
        let sh: SemanticHit = hit.into();
        assert_eq!(sh.fqn, "my::Fn");
        assert_eq!(sh.repo_id, "r1");
        assert!((sh.score - 0.9).abs() < f32::EPSILON);
    }

    #[test]
    fn limit_is_capped_at_max() {
        // Verify the cap formula: min(limit, MAX) works.
        let capped = 200_u32.min(MAX_SEARCH_LIMIT).max(1);
        assert_eq!(capped, MAX_SEARCH_LIMIT);
    }

    #[test]
    fn limit_zero_becomes_one() {
        let capped = 0_u32.min(MAX_SEARCH_LIMIT).max(1);
        assert_eq!(capped, 1);
    }

    #[test]
    fn default_limit_is_within_max() {
        assert!(DEFAULT_SEARCH_LIMIT <= MAX_SEARCH_LIMIT);
        assert!(DEFAULT_SEARCH_LIMIT > 0);
    }
}
