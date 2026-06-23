//! LLM multi-query rewrite for hybrid retrieval (ADR-014 §6, Wave 10 S5).
//!
//! Entry point: [`expand_query`]. Returns `[original]` when n=1 or force-off is set,
//! otherwise calls local Ollama to generate up to `n-1` paraphrases and prepends the
//! original so the caller always has at least one query variant.
//!
//! Cost ceiling: global cap [`MAX_MULTI_QUERY_N`] = 3 is enforced at [`resolve_n`].
//! Per-tenant token budget is enforced inside [`expand_query`]: calls that would exceed
//! the budget short-circuit to `[original]` and emit a Prometheus counter.

use reqwest::Client;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum number of query variants (ADR-014 §9). Resolver clamps above this.
pub const MAX_MULTI_QUERY_N: u32 = 3;

// ---------------------------------------------------------------------------
// Per-tenant configuration
// ---------------------------------------------------------------------------

/// Resolved per-tenant multi-query rewrite configuration.
///
/// Build with [`MultiQueryConfig::default`] (n=1, disabled) for the no-rewrite path,
/// or load from `control.tenant_query_settings` for per-tenant overrides.
#[derive(Debug, Clone)]
pub struct MultiQueryConfig {
    /// Effective number of query variants (including the original). Already clamped to
    /// `[1, MAX_MULTI_QUERY_N]` by [`resolve_n`]. When `n == 1`, `expand_query` returns
    /// `[original]` without calling Ollama.
    pub n: u32,
    /// When `true`, expansion is forced off for this tenant regardless of `n`.
    /// `force_off` always wins over the global default (ADR-014 §6).
    pub force_off: bool,
    /// Per-tenant token budget for the LLM rewrite call.
    /// `0` means disabled — `expand_query` short-circuits to `[original]`.
    pub token_budget: u32,
}

impl Default for MultiQueryConfig {
    fn default() -> Self {
        Self {
            n: 1,
            force_off: false,
            token_budget: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Ollama API types (typed to avoid serde_json::Value in non-dev code)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct OllamaGenerateRequest<'a> {
    model: &'a str,
    prompt: &'a str,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaOptions {
    num_predict: u32,
}

#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: Option<String>,
    eval_count: Option<u64>,
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

/// Resolve the effective n for a tenant request.
///
/// Applies the global cap and the force-off flag:
/// - `force_off == true` → returns 1 (original only), regardless of any n.
/// - otherwise: `min(tenant_n, MAX_MULTI_QUERY_N)`.
#[must_use]
pub fn resolve_n(tenant_n: u32, force_off: bool) -> u32 {
    if force_off {
        return 1;
    }
    tenant_n.min(MAX_MULTI_QUERY_N)
}

// ---------------------------------------------------------------------------
// Expand
// ---------------------------------------------------------------------------

/// Expand `query` into up to `config.n` variants via local Ollama.
///
/// # Returns
///
/// - `[original]` when `config.n == 1`, `config.force_off == true`, or
///   `config.token_budget == 0` (AC4 — disabled by default in v1).
/// - `[original, paraphrase_1, …]` otherwise, with the original always first.
///
/// # Failure modes (all short-circuit to `[original]`)
///
/// - Ollama is unreachable or returns non-2xx.
/// - Response missing the expected text field.
/// - Response token count exceeds `token_budget` (emits `rb_query_rewrite_over_budget_total`).
pub async fn expand_query(
    config: &MultiQueryConfig,
    http: &Client,
    ollama_url: &str,
    model: &str,
    query: &str,
) -> Vec<String> {
    let effective_n = resolve_n(config.n, config.force_off);

    // Short-circuit: n=1, force-off, or token budget disabled.
    if effective_n <= 1 || config.token_budget == 0 {
        return vec![query.to_owned()];
    }

    let want_paraphrases = (effective_n - 1) as usize;
    let prompt = build_rewrite_prompt(query, want_paraphrases);

    let url = format!("{}/api/generate", ollama_url.trim_end_matches('/'));
    let body = OllamaGenerateRequest {
        model,
        prompt: &prompt,
        stream: false,
        options: OllamaOptions {
            num_predict: config.token_budget,
        },
    };

    let resp = match http.post(&url).json(&body).send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::warn!(status = %r.status(), "Ollama rewrite returned non-2xx; falling back to original");
            return vec![query.to_owned()];
        }
        Err(e) => {
            tracing::warn!(error = %e, "Ollama rewrite request failed; falling back to original");
            return vec![query.to_owned()];
        }
    };

    let parsed: OllamaGenerateResponse = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Ollama rewrite response parse error; falling back to original");
            return vec![query.to_owned()];
        }
    };

    // Check token usage against per-tenant budget.
    if let Some(used) = parsed.eval_count {
        if used > u64::from(config.token_budget) {
            metrics::counter!("rb_query_rewrite_over_budget_total").increment(1);
            tracing::warn!(
                used,
                budget = config.token_budget,
                "rewrite over token budget; falling back to original"
            );
            return vec![query.to_owned()];
        }
    }

    let Some(raw) = parsed.response else {
        tracing::warn!(
            "Ollama rewrite response missing 'response' field; falling back to original"
        );
        return vec![query.to_owned()];
    };

    let paraphrases = parse_paraphrases(&raw, want_paraphrases);
    if paraphrases.is_empty() {
        return vec![query.to_owned()];
    }

    let mut variants = Vec::with_capacity(1 + paraphrases.len());
    variants.push(query.to_owned());
    variants.extend(paraphrases);
    variants
}

// ---------------------------------------------------------------------------
// Helpers (private)
// ---------------------------------------------------------------------------

fn build_rewrite_prompt(query: &str, n_paraphrases: usize) -> String {
    format!(
        "Generate exactly {n_paraphrases} alternative phrasings of the following search query \
         for a Rust source-code search engine. Output ONLY the alternatives, one per line, \
         with no numbering, no punctuation prefix, and no explanation.\n\nQuery: {query}"
    )
}

fn parse_paraphrases(raw: &str, limit: usize) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(limit)
        .map(ToOwned::to_owned)
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // AC5: global cap enforced by resolve_n.
    #[test]
    fn resolve_n_clamps_to_max() {
        assert_eq!(resolve_n(10, false), MAX_MULTI_QUERY_N);
        assert_eq!(resolve_n(3, false), 3);
        assert_eq!(resolve_n(2, false), 2);
        assert_eq!(resolve_n(1, false), 1);
    }

    // AC3 + AC5: force_off always wins.
    #[test]
    fn resolve_n_force_off_wins() {
        assert_eq!(resolve_n(3, true), 1);
        assert_eq!(resolve_n(1, true), 1);
        assert_eq!(resolve_n(MAX_MULTI_QUERY_N + 1, true), 1);
    }

    // AC4: token_budget == 0 → short-circuit without Ollama call.
    #[test]
    fn config_default_is_disabled() {
        let cfg = MultiQueryConfig::default();
        assert_eq!(cfg.n, 1);
        assert!(!cfg.force_off);
        assert_eq!(cfg.token_budget, 0);
    }

    #[test]
    fn parse_paraphrases_extracts_lines() {
        let raw = "first alternative\nsecond alternative\nthird alternative\n";
        let result = parse_paraphrases(raw, 2);
        assert_eq!(result, vec!["first alternative", "second alternative"]);
    }

    #[test]
    fn parse_paraphrases_ignores_blank_lines() {
        let raw = "\nfirst\n\nsecond\n";
        let result = parse_paraphrases(raw, 3);
        assert_eq!(result, vec!["first", "second"]);
    }

    #[test]
    fn parse_paraphrases_empty_returns_empty() {
        assert!(parse_paraphrases("", 3).is_empty());
    }

    #[test]
    fn build_rewrite_prompt_contains_query() {
        let prompt = build_rewrite_prompt("find authentication errors", 2);
        assert!(prompt.contains("find authentication errors"));
        assert!(prompt.contains('2'));
    }

    // AC7: when n=1 and force_off=false, expand_query must return [original].
    // Validated synchronously through resolve_n (the async path is integration-tested).
    #[test]
    fn n_equals_1_resolves_to_original_only() {
        let n = resolve_n(1, false);
        assert_eq!(n, 1, "n=1 must short-circuit without Ollama call");
    }

    // AC7 variant: force_off=true also short-circuits.
    #[test]
    fn force_off_resolves_to_original_only() {
        let n = resolve_n(2, true);
        assert_eq!(n, 1);
    }
}
