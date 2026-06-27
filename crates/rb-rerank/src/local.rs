//! Local ONNX cross-encoder reranker using `fastembed`.
//!
//! The ONNX model artifact must be pre-placed at `RB_RERANK_MODEL_DIR`
//! (default `/models/rerank`). fastembed checks the cache dir first and
//! will skip downloading if the model is already present — required for
//! self-hosted deployments with no managed CDN (ADR-014 §12).
//!
//! **Model path**: `$RB_RERANK_MODEL_DIR/Xenova--bge-reranker-base/` (fastembed
//! stores models under `<author>--<model>` slug directories).

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use fastembed::{RerankInitOptions, RerankerModel, TextRerank};

use crate::{
    error::RerankerError,
    reranker::{RankedResult, RerankCandidate, Reranker},
};

/// In-process cross-encoder reranker backed by a local ONNX model via `fastembed`.
///
/// Wraps `fastembed::TextRerank` in an `Arc` so the model is loaded once and
/// can be cloned cheaply into `spawn_blocking` tasks.
pub struct LocalCrossEncoder {
    inner: Arc<TextRerank>,
}

impl LocalCrossEncoder {
    /// Load the BGE-reranker-base ONNX model from `cache_dir`.
    ///
    /// Blocking at call time: ONNX session initialisation can take 100–500 ms.
    /// Call once at server startup and store the result in [`AppState`].
    ///
    /// # Errors
    ///
    /// Returns [`RerankerError::Model`] if the model directory is missing or the
    /// ONNX session fails to initialise.
    pub fn try_new(cache_dir: impl Into<PathBuf>) -> Result<Self, RerankerError> {
        // RerankInitOptions is #[non_exhaustive] in fastembed — use Default + field mutation.
        let mut options = RerankInitOptions::default();
        options.model_name = RerankerModel::BGERerankerBase;
        options.show_download_progress = false;
        options.cache_dir = cache_dir.into();
        let model =
            TextRerank::try_new(options).map_err(|e| RerankerError::Model(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(model),
        })
    }
}

#[async_trait]
impl Reranker for LocalCrossEncoder {
    /// Rerank `candidates` against `query` using the local ONNX cross-encoder.
    ///
    /// The heavy ONNX inference runs in a `spawn_blocking` thread so it does
    /// not block the async executor. Returns results sorted descending by score.
    async fn rerank(
        &self,
        query: &str,
        candidates: Vec<RerankCandidate>,
    ) -> Result<Vec<RankedResult>, RerankerError> {
        if candidates.is_empty() {
            return Ok(vec![]);
        }

        let model = Arc::clone(&self.inner);
        let query_owned = query.to_owned();
        // Preserve position mapping so we can round-trip back to original_idx.
        let original_indices: Vec<usize> = candidates.iter().map(|c| c.original_idx).collect();
        let texts: Vec<String> = candidates.into_iter().map(|c| c.text).collect();

        // fastembed inference is CPU-bound; run outside the async executor.
        let raw = tokio::task::spawn_blocking(move || {
            // fastembed 4.x takes Vec<&String>; collect from the owned vec inside the closure
            // so the borrows stay valid within the 'static closure.
            let refs: Vec<&String> = texts.iter().collect();
            model.rerank(&query_owned, refs, false, None)
        })
        .await
        .map_err(|e| RerankerError::Blocking(e.to_string()))?
        .map_err(|e| RerankerError::Model(e.to_string()))?;

        // fastembed returns results sorted descending by score; map back to caller's indices.
        Ok(raw
            .into_iter()
            .map(|r| RankedResult {
                original_idx: original_indices[r.index],
                rerank_score: r.score,
            })
            .collect())
    }
}

// ---------------------------------------------------------------------------
// AC8 latency microbench (run with `cargo test --ignored`)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod bench {
    use super::*;

    /// AC8 — local cross-encoder over N=50 must stay within the §9 end-to-end budget.
    ///
    /// Requires the ONNX model to be pre-downloaded at `RB_RERANK_MODEL_DIR`.
    /// Run on dev hardware:
    ///   `cargo test -p rb-rerank -- --ignored bench_local_cross_encoder_n50`
    ///
    /// Budget per ADR-014 §9: p50 ≤ 350 ms end-to-end.
    /// Record the printed p50 in the PR description.
    #[test]
    #[ignore = "requires ONNX model at RB_RERANK_MODEL_DIR; run --ignored on dev hardware"]
    fn bench_local_cross_encoder_n50() {
        const ITERS: u32 = 10;

        let model_dir =
            std::env::var("RB_RERANK_MODEL_DIR").unwrap_or_else(|_| "/models/rerank".to_owned());

        let encoder = LocalCrossEncoder::try_new(&model_dir).expect("model load failed");

        let query = "parse abstract syntax tree";
        let make_candidates = || -> Vec<RerankCandidate> {
            (0..50)
                .map(|i| RerankCandidate {
                    original_idx: i,
                    text: format!("crate_{i}::parser::parse_node"),
                    original_score: 0.5,
                })
                .collect()
        };

        let rt = tokio::runtime::Runtime::new().unwrap();

        // Warm-up: first inference may trigger JIT / ONNX graph optimisation.
        rt.block_on(encoder.rerank(query, make_candidates()))
            .expect("warm-up rerank must succeed");
        let start = std::time::Instant::now();
        for _ in 0..ITERS {
            rt.block_on(encoder.rerank(query, make_candidates()))
                .expect("rerank must succeed");
        }
        let p50_ms = start.elapsed().as_millis() / u128::from(ITERS);

        println!("LocalCrossEncoder N=50  p50={p50_ms}ms  (budget: ≤350ms)");

        assert!(
            p50_ms <= 350,
            "AC8 FAIL: LocalCrossEncoder N=50 p50={p50_ms}ms exceeds §9 budget of 350ms"
        );
    }
}
