//! Per-tenant cost ceiling helpers for the retrieval pipeline (ADR-014 §9, S7).

/// Enforce the rerank candidate cap. Returns `candidates` unchanged when
/// `cap == 0` (sentinel: unconfigured). Emits warning + counter when clamped.
pub(crate) fn clamp_rerank_candidates<T>(
    candidates: Vec<T>,
    cap: u32,
    tenant_id: uuid::Uuid,
) -> Vec<T> {
    let cap = cap as usize;
    if cap == 0 || candidates.len() <= cap {
        return candidates;
    }
    tracing::warn!(
        tenant_id = %tenant_id,
        original = candidates.len(),
        cap,
        "rerank candidate set clamped to cap"
    );
    metrics::counter!(
        "retrieval_rerank_clamped_total",
        "tenant_id" => tenant_id.to_string(),
    )
    .increment(1);
    candidates.into_iter().take(cap).collect()
}

/// Check whether this tenant has remaining LLM token budget.
///
/// When `ceiling == 0` (default), all LLM calls are short-circuited → zero cost.
/// Returns `true` when the call is allowed, `false` when budget is exhausted.
pub(crate) fn llm_budget_allows(ceiling: u32, tokens_used: u32, tenant_id: uuid::Uuid) -> bool {
    if ceiling == 0 {
        return false;
    }
    if tokens_used >= ceiling {
        metrics::counter!(
            "llm_budget_exceeded_total",
            "tenant_id" => tenant_id.to_string(),
        )
        .increment(1);
        return false;
    }
    true
}
