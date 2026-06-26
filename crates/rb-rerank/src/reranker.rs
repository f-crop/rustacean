use async_trait::async_trait;

use crate::error::RerankerError;

/// A single candidate fed to the reranker.
#[derive(Debug, Clone)]
pub struct RerankCandidate {
    /// Position of this candidate in the caller's hit list.
    pub original_idx: usize,
    /// Text to score against the query (typically an FQN or source excerpt).
    pub text: String,
    /// Score from the upstream retrieval leg; used as tiebreak only.
    pub original_score: f32,
}

/// One entry in the reranker's output, sorted descending by `rerank_score`.
#[derive(Debug, Clone)]
pub struct RankedResult {
    /// Index from [`RerankCandidate::original_idx`].
    pub original_idx: usize,
    /// Cross-encoder relevance score in `[0, 1]`.
    pub rerank_score: f32,
}

/// Scores and re-orders a list of candidate texts against a query.
///
/// Implementations must be `Send + Sync` so they can be stored in [`std::sync::Arc`]
/// and called from any async task (e.g. via `spawn_blocking`).
///
/// **Tenant isolation contract**: the trait accepts only raw text — it has no
/// access to tenant IDs, database pools, or Qdrant. The isolation guarantee
/// therefore reduces to: the *input* set must already be single-tenant, which
/// is enforced by `rb_query::hybrid_search` (ADR-014 §10, AC5).
#[async_trait]
pub trait Reranker: Send + Sync {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
    ) -> Result<Vec<RankedResult>, RerankerError>;
}
