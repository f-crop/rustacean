//! `rb-rerank` — flag-gated cross-encoder reranker for Wave 10 hybrid retrieval.
//!
//! Provides two implementations of the [`Reranker`] trait:
//! - [`LocalCrossEncoder`]: ONNX BGE-reranker-base via `fastembed` (default).
//! - [`LlmReranker`]: local Ollama LLM "quality mode" (per-tenant opt-in).
//!
//! **Crate graph**: leaf crate — depends only on `fastembed` + workspace deps.
//! `rb-query` does NOT depend on this crate (no cycle, ADR-014 §2).

mod error;
mod llm;
mod local;
mod reranker;

pub use error::RerankerError;
pub use llm::LlmReranker;
pub use local::LocalCrossEncoder;
pub use reranker::{RankedResult, RerankCandidate, Reranker};

#[cfg(test)]
mod tests {
    use super::*;

    /// AC5 regression proof: the `Reranker` trait signature must contain no
    /// tenant ID parameter.  If anyone adds `tenant_id` to the trait, this
    /// struct-level compile-time test will fail to build, surfacing the
    /// isolation violation immediately.
    ///
    /// Tenant isolation is enforced by `rb_query::hybrid_search` BEFORE the
    /// reranker is invoked — the reranker only sees already-filtered hits.
    #[test]
    fn reranker_trait_has_no_tenant_parameter() {
        fn assert_no_tenant<T: Reranker>() {}

        struct TenantFreeReranker;

        #[async_trait::async_trait]
        impl Reranker for TenantFreeReranker {
            async fn rerank(
                &self,
                _query: &str,
                candidates: Vec<RerankCandidate>,
            ) -> Result<Vec<RankedResult>, RerankerError> {
                Ok(candidates
                    .into_iter()
                    .map(|c| RankedResult {
                        original_idx: c.original_idx,
                        rerank_score: c.original_score,
                    })
                    .collect())
            }
        }

        assert_no_tenant::<TenantFreeReranker>();
    }
}
