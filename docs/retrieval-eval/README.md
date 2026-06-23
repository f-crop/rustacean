# Retrieval Eval Harness

Offline regression gate for hybrid search quality (ADR-014 §8, Wave 10 S6).

## Overview

CI reads a pre-computed `results-snapshot.json` and evaluates four metrics against golden
Q-A fixtures. No live services are required. The gate fails if quality drops below thresholds.

**Cutover gate**: prod flag `RB_HYBRID_SEARCH_ENABLED` may not be flipped until the
`retrieval-eval` CI job passes (ADR-014 §7.3.b).

---

## Directory layout

```
docs/retrieval-eval/
├── golden/
│   └── rust-brain-by-gov.json   # hand-crafted Q-A fixtures
├── results-snapshot.json         # pre-computed hybrid_search results (seed=42)
├── baseline.json                 # committed metric baseline
└── README.md                     # this file
scripts/
└── eval-retrieval.py             # runner (ci mode + regenerate mode)
.github/workflows/
├── ci.yml                        # retrieval-eval job
└── regenerate-eval-baseline.yml  # workflow_dispatch to regenerate snapshot+baseline
```

---

## Golden fixture schema

File: `golden/<repo-slug>.json` — a JSON array of query objects.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `string` | Unique query id, e.g. `q-001` |
| `query` | `string` | Natural-language question |
| `repo_id` | `string` | UUID of the target repo (must match ingestion) |
| `relevant_chunks` | `string[]` | FQNs with `sym:` prefix, e.g. `sym:rb_query::hybrid::rrf_fuse` |
| `notes` | `string` | Human explanation of why these chunks are relevant |

### FQN format

FQNs follow Rust definition paths: `sym:<crate>::<module>::<item>`.

Examples:
- `sym:rb_query::hybrid::rrf_fuse`
- `sym:rb_schemas::citation::CitationV1`
- `sym:control_api::routes::query::search::embed_query`

---

## Metrics

| Metric | Formula | Threshold |
|--------|---------|-----------|
| Recall@5 | \|top-5 ∩ relevant\| / \|relevant\| | informational |
| Recall@10 | \|top-10 ∩ relevant\| / \|relevant\| | **zero tolerance — any drop fails CI** |
| MRR | 1 / rank of first relevant result | informational |
| nDCG@10 | DCG@10 / IDCG@10 (binary relevance) | **drop > 0.02 fails CI** |
| Citation-P@10 | \|top-10 ∩ relevant\| / 10 | informational |

DCG uses the standard formula: `sum(1 / log2(rank + 1))` for each relevant result in top-k.
IDCG is the ideal DCG if all relevant chunks appeared at ranks 1…|relevant|.

---

## Determinism

The snapshot was generated with `--seed 42` (documented in `results-snapshot.json`). The
eval runner itself has no RNG — all metric computation is deterministic. Re-generating the
snapshot with the same seed must produce identical results.

---

## How to add a Q-A entry

1. Pick a question whose answer is clearly defined by one or more symbols in the codebase.
2. Run the symbols through `cargo grep` or rust-analyzer to confirm their canonical FQN.
3. Add a new object to `golden/rust-brain-by-gov.json` with the next sequential `id`.
4. Run `python3 scripts/eval-retrieval.py --mode regenerate` locally (requires services).
5. Commit both the updated golden file and the new `results-snapshot.json` / `baseline.json`.
6. Check the baseline diff is reasonable — adding a harder query will lower some metrics.

---

## Regeneration procedure

The `regenerate` mode calls the live `hybrid_search` API for each golden query and writes
new `results-snapshot.json` and `baseline.json` files.

```bash
# Requires: RB_HYBRID_SEARCH_ENABLED=true, services running locally
python3 scripts/eval-retrieval.py --mode regenerate \
  --api-url http://localhost:8080 \
  --api-key "$RB_API_KEY"
```

CI automation: trigger the `regenerate-eval-baseline` workflow via `workflow_dispatch` on
GitHub Actions. **PRs that update baseline.json must check the reviewer acknowledgement
checkbox in the PR template.**

---

## CI job

The `retrieval-eval` job runs when any of these paths change:

- `crates/rb-query/**`
- `crates/rb-rerank/**`
- `crates/rb-schemas/**`
- `services/control-api/src/routes/query/**`
- `docs/retrieval-eval/**`
- `scripts/eval-retrieval.py`

It runs unconditionally on every push to `main`.

Exit codes:
- `0` — all metrics at or above baseline thresholds
- `1` — one or more thresholds breached (GitHub Actions annotations emitted for each failure)
