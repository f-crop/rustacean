# Retrieval Metrics — Observability Runbook

Wave 10 S7 (ADR-014 §9). All metrics emitted by `control-api` via the `metrics` facade (Prometheus exporter on `GET /metrics`).

## Metric Catalogue

### `retrieval_request_duration_ms` (histogram)

End-to-end retrieval latency from query received to ranked results serialized, measured in milliseconds.

| Label | Values |
|-------|--------|
| `mode` | `dense` · `hybrid` · `hybrid_rerank` |

Emission points:
- `dense` — emitted in `search.rs` and `dispatch.rs` after `semantic_search` returns.
- `hybrid` — emitted in `search.rs` and `dispatch.rs` after `hybrid_search` returns.
- `hybrid_rerank` — reserved for when the cross-encoder reranker is wired in (S8+). Not yet emitted.

**ADR-014 §9 latency budgets (warm caches, dev hardware):**

| Mode             | p50     | p95     | p99      |
|------------------|---------|---------|----------|
| `dense`          | ≤120 ms | ≤300 ms | ≤500 ms  |
| `hybrid`         | ≤180 ms | ≤400 ms | ≤700 ms  |
| `hybrid_rerank`  | ≤350 ms | ≤800 ms | ≤1200 ms |

**Alert threshold (example Prometheus rule):**
```yaml
- alert: RetrievalLatencyBudgetExceeded
  expr: |
    histogram_quantile(0.95,
      rate(retrieval_request_duration_ms_bucket[5m])
    ) > 400
  for: 5m
  labels:
    severity: warning
  annotations:
    summary: "hybrid retrieval p95 exceeds 400 ms budget"
```

---

### `retrieval_candidates_total` (counter)

Cumulative count of result candidates returned by the retrieval path before post-processing.

| Label | Values |
|-------|--------|
| `mode` | `dense` · `hybrid` · `hybrid_rerank` |

**Expected range:** 0–50 per request (capped by `MIN_FETCH` / `rerank_candidate_cap`).

---

### `retrieval_rerank_clamped_total` (counter)

Incremented when a rerank candidate set exceeds `RB_RERANK_CANDIDATE_CAP` (default 50) and is truncated. Non-zero values indicate either a misconfigured cap or callers attempting to bypass it.

| Label | Values |
|-------|--------|
| `tenant_id` | UUID of the requesting tenant |

**Expected range:** 0 (should never fire with default cap=50 and MIN_FETCH=50).

---

### `llm_budget_exceeded_total` (counter)

Incremented when a tenant's LLM token budget (`RB_LLM_TOKEN_CEILING_PER_TENANT`) is exhausted and an LLM call is short-circuited.

| Label | Values |
|-------|--------|
| `tenant_id` | UUID of the requesting tenant |

**Default:** `RB_LLM_TOKEN_CEILING_PER_TENANT=0` means all LLM calls are disabled → counter is always 0 for tenants without an explicit ceiling configured.

**Alert threshold:**
```yaml
- alert: LlmBudgetExhausted
  expr: increase(llm_budget_exceeded_total[1h]) > 0
  for: 0m
  labels:
    severity: info
  annotations:
    summary: "tenant {{ $labels.tenant_id }} hit LLM token ceiling"
```

---

## Config Reference

| Env Var | Default | Description |
|---------|---------|-------------|
| `RB_RERANK_CANDIDATE_CAP` | `50` | Max candidates passed to the cross-encoder reranker. |
| `RB_MULTI_QUERY_MAX` | `3` | Max parallel sub-queries for multi-query expansion. |
| `RB_LLM_TOKEN_CEILING_PER_TENANT` | `0` | Per-tenant LLM token ceiling. **0 = disabled (zero LLM cost).** |

## Load Test

The `integration_retrieval_perf_tests` fixture in `services/control-api/tests/` asserts that the Postgres FTS leg meets a conservative sub-budget. Run locally with:

```bash
RB_DATABASE_URL=postgres://... cargo test -p control-api \
  --test integration_retrieval_perf_tests -- --nocapture
```

The `retrieval-perf` CI job runs this automatically on PRs that touch the retrieval surface.

## Parallel Legs

As of S7, `hybrid_search` runs the dense (Qdrant ANN) and sparse (Postgres FTS) legs concurrently via `tokio::try_join!`. If either leg fails the entire call returns an error; partial results are not served.
