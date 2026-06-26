//! Optional LLM-backed reranker using a local Ollama instance.
//!
//! This is the "quality mode" reranker (ADR-014 §4): off by default, enabled
//! only with the per-tenant toggle (S6 will add the DB-level toggle; for the
//! S3 tracer-bullet this is controlled by `RB_RERANK_LLM_ENABLED`).
//!
//! For each candidate the reranker prompts the LLM to return a relevance score
//! in `[0, 1]`. Scores are computed sequentially and sorted descending.
//! Candidates that fail scoring fall back to their original retrieval score.

use async_trait::async_trait;

use crate::{
    error::RerankerError,
    reranker::{RankedResult, RerankCandidate, Reranker},
};

/// LLM-backed reranker via the local Ollama `/api/generate` endpoint.
///
/// Sequential by design for the S3 tracer-bullet; S7 may add concurrent
/// batching. The latency is bounded by `N_candidates × LLM_latency_per_pair`.
pub struct LlmReranker {
    http: reqwest::Client,
    ollama_url: String,
    model: String,
}

impl LlmReranker {
    pub fn new(
        http: reqwest::Client,
        ollama_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            http,
            ollama_url: ollama_url.into(),
            model: model.into(),
        }
    }

    async fn score_pair(&self, query: &str, text: &str) -> f32 {
        let prompt = format!(
            "Rate the relevance of the following code symbol to the search query.\n\
             Respond with ONLY a decimal number between 0.0 and 1.0 and nothing else.\n\
             Query: {query}\n\
             Code symbol: {text}\n\
             Relevance score:"
        );

        let url = format!("{}/api/generate", self.ollama_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": self.model,
            "prompt": prompt,
            "stream": false,
        });

        let Ok(resp) = self.http.post(&url).json(&body).send().await else {
            tracing::warn!(model = %self.model, "LLM reranker HTTP request failed");
            return 0.5;
        };

        let Ok(json) = resp.json::<serde_json::Value>().await else {
            tracing::warn!(model = %self.model, "LLM reranker response parse failed");
            return 0.5;
        };

        let raw = json
            .get("response")
            .and_then(|v| v.as_str())
            .unwrap_or("0.5");

        raw.trim().parse::<f32>().map_or(0.5, |s| s.clamp(0.0, 1.0))
    }
}

#[async_trait]
impl Reranker for LlmReranker {
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
    ) -> Result<Vec<RankedResult>, RerankerError> {
        let mut results = Vec::with_capacity(candidates.len());

        for c in &candidates {
            let score = self.score_pair(query, &c.text).await;
            results.push(RankedResult {
                original_idx: c.original_idx,
                rerank_score: score,
            });
        }

        results.sort_by(|a, b| {
            b.rerank_score
                .partial_cmp(&a.rerank_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(results)
    }
}
