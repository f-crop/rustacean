# ADR-014: Hybrid Retrieval & Knowledge Quality (Wave 10)

**Status**: Proposed (Gate-1, awaiting board acceptance)
**Date**: 2026-06-22
**Author**: Architect
**Supersedes**: none. **Extends**: ADR-007 (semantic graph model & tenant isolation §13.2), ADR-013 (MCP/runtime contract).
**Wave**: 10 · Stream S1. This ADR is the contract that S2–S7 implement against.

---

## 1. Context

Today retrieval is **dense-only**. The path is:

```
query --normalize--> Ollama (nomic-embed-text, 768-d) --vector-->
  rb_query::semantic_search --> rb_storage_qdrant::TenantVectorStore::search
    (must-filter tenant_id [+ repo_id]) --> rb_embeddings collection --> ranked SemanticHit{fqn, repo_id, score}
```

Two call sites embed and search:

- `services/control-api/src/routes/query/search.rs` (`POST /v1/search`, REQ-DP-01)
- `services/control-api/src/routes/mcp/dispatch.rs` (`search_items` MCP tool)

Forces in play:

- **Lexical recall gap.** Pure dense recall misses exact-token queries (error strings, symbol names, log lines, config keys) where embeddings smear precise tokens. Code search is unusually token-literal.
- **No citations.** Results return `{fqn, repo_id, score}` only — no file path, line range, or commit SHA. The Wave 10 chat UI (S4) needs a stable, versioned citation envelope.
- **No eval.** We have no golden set and no regression guard; quality changes are unmeasured.
- **No FTS infrastructure.** `code_symbols` (per-tenant PG schema: `fqn, kind, signature, source_text, line_start, line_end, repo_id`) has no full-text index. `file_path` is reachable via `code_files.relative_path`; `commit_sha` is **not** currently stored on the symbol.

### Hard constraints (from the epic, binding)

1. **Self-hosted only** — no managed reranker/embedding services. Ollama or a local crate.
2. **Reversible** — hybrid behind a feature flag; default **off** in dev, **on** in prod only after S2 lands **and** S6 eval clears.
3. **No upstream Paperclip changes.**
4. **No new external dependency** without explicit ADR justification (see §12).
5. **Tenant isolation preserved** on **both** `embed_query` sites.

---

## 2. Decision Summary

| # | Decision | Choice |
|---|----------|--------|
| 1 | Sparse representation | **Postgres FTS** (`tsvector` GENERATED column + GIN index) on `code_symbols`, ranked with `ts_rank_cd` (BM25-style). No new infra. |
| 1 | Dense representation | **Keep** `nomic-embed-text` 768-d on Qdrant. No embedding upgrade in Wave 10 (separate reversible change). |
| 1 | Fusion | **Reciprocal Rank Fusion (RRF)**, `k = 60` default, parameter-free. |
| 2 | Reranker | **In-process, optional, flag-gated.** New `rb-rerank` crate hosting a local ONNX cross-encoder (default reranker). LLM-rerank offered as opt-in "quality mode". Default **off**. |
| 3 | Citation contract | Versioned `CitationV1` envelope (§5). |
| 4 | Query understanding | LLM multi-query rewrite, **default 1 (no rewrite)** in v1; per-tenant disable hook via `rb-feature-resolver`. |
| 5 | Eval harness | Golden Q-A fixtures; Recall@k, MRR, nDCG@10, citation-precision; CI regression threshold (§8). |
| 6 | Latency/cost budgets | Numeric p50/p95/p99 per mode + per-tenant rerank/expand ceilings (§9). |
| 7 | Index migration | Additive GENERATED `tsvector` column + GIN index; per-tenant cutover gated on backfill + eval. Dense index untouched → fully reversible. |
| 8 | Tenant isolation | Single `rb_query::hybrid::hybrid_search(tenant, …)` entry point; **both** call sites migrate to it; cross-tenant leakage regression test on both legs. |

### Component map (one-way `crates ← services` preserved)

```
services/control-api ──► crates/rb-query ──► crates/rb-storage-qdrant   (dense leg, unchanged)
                     │                   └──► crates/rb-query::pg FTS    (sparse leg, new)
                     └──► crates/rb-rerank (NEW crate)                   (optional rerank stage)
                     └──► crates/rb-feature-resolver                     (flag + per-tenant toggle, reused)
```

No crate depends on a service. No new `rb-* ↔ rb-*` cycle: `rb-rerank` is leaf (depends only on `rb-schemas` + the reranker crate); `rb-query` gains a `hybrid` module that *optionally* invokes `rb-rerank` — to avoid a cycle, fusion lives in `rb-query` and rerank is applied by the **caller** (control-api) after fusion, so `rb-query` does **not** depend on `rb-rerank`.

---

## 3. Hybrid search strategy (decision 1)

### 3.1 Sparse leg — Postgres FTS

`code_symbols` already lives in a per-tenant PG schema (`TenantCtx::qualify`), so it is **inherently tenant-scoped** — no extra filter needed. We add a generated text-search column over the lexically meaningful fields and a GIN index:

```sql
-- tenant migration 007 (additive)
ALTER TABLE code_symbols
  ADD COLUMN IF NOT EXISTS fts tsvector
  GENERATED ALWAYS AS (
    to_tsvector('simple',
      coalesce(fqn,'') || ' ' || coalesce(signature,'') || ' ' || coalesce(source_text,''))
  ) STORED;
CREATE INDEX IF NOT EXISTS idx_code_symbols_fts ON code_symbols USING GIN (fts);
```

GENERATED STORED means the column self-maintains on insert/update — **no backfill drift**, no application change to the projector. Ranking uses `ts_rank_cd(fts, plainto_tsquery('simple', $q))`. The `'simple'` config avoids language-stemming that hurts identifiers; `plainto_tsquery` is injection-safe (parameterized).

### 3.2 Dense leg — unchanged

`semantic_search` → `TenantVectorStore::search` stays as-is. The mandatory `tenant_id` `must`-filter is the isolation guarantee (ADR-007 §13.2).

### 3.3 Fusion — RRF

Each leg returns a ranked list; final score for doc *d*:

```
RRF(d) = Σ_legs  1 / (k + rank_leg(d)),   k = 60
```

Run both legs to depth `N_fetch = max(limit, 50)`, fuse, truncate to `limit`. RRF is **parameter-free** (no per-tenant score tuning), robust to the cosine-vs-`ts_rank` scale mismatch, and the standard baseline. `k=60` is configurable but fixed for v1.

---

## 4. Reranker placement (decision 2)

- **Placement: in-process**, applied by control-api on the fused top-`N` (`N ≤ 50`) *after* `rb_query::hybrid_search` returns. Avoids a new network hop/deploy unit and keeps `rb-query` free of a rerank dependency (no cycle).
- **Default reranker: local cross-encoder** in a new `crates/rb-rerank` crate, running an ONNX model (e.g. `bge-reranker-base`) via `fastembed` (the single new dependency — see §12). Self-hosted, no managed service.
- **LLM-rerank**: opt-in "quality mode" via the local Ollama LLM, behind the same flag + a per-tenant toggle. **Not** the default (latency/cost unbounded).
- **Default: OFF.** Rerank turns on only with the hybrid flag, and LLM-rerank only with an explicit per-tenant opt-in.
- **Latency budget**: rerank over `N=50` candidates must stay within the §9 hybrid+rerank envelope.

---

## 5. Citation contract (decision 3 — consumed by S4)

Stable, versioned envelope. S4 renders against `version`; new fields are additive within a version, breaking changes bump the version.

```jsonc
// CitationV1
{
  "version": "v1",
  "repo_id": "uuid",
  "file_path": "src/foo/bar.rs",      // from code_files.relative_path
  "line_range": { "start": 12, "end": 48 },  // code_symbols.line_start/line_end
  "commit_sha": "abc123…",            // see note below
  "score": 0.0,                        // fused/reranked score, normalized [0,1]
  "source_kind": "hybrid"             // "dense" | "sparse" | "hybrid" | "rerank"
}
```

**Data sourcing note (binding on S2):** `file_path` requires a `code_files` join on `repo_id`; `commit_sha` is **not** on `code_symbols` today. S2 must source it from the repo's ingested snapshot (the commit that produced the projection). If a per-symbol commit SHA is impractical in Wave 10, S2 records the **repo head SHA at ingest time** and documents the granularity in the response; the envelope field is mandatory and non-null. The envelope is defined in `rb-schemas` so every consumer (S4 UI, MCP) shares one type.

---

## 6. Query understanding (decision 4 — consumed by S5)

- **Expansion strategy: LLM multi-query rewrite** (generate ≤3 paraphrases via local Ollama LLM), **not** synonym dictionaries (code-domain synonyms are brittle and low-value).
- **Default multi-query = 1 (no rewrite) in v1.** Rewrite is wired but disabled until S6 eval shows it helps; this keeps the tracer slice cheap and measurable.
- **Per-tenant disable hook** via `rb-feature-resolver` (already in the workspace): a tenant can force expansion off regardless of the global default.
- When multi-query > 1, each variant runs the full hybrid path; results are RRF-fused across variants and legs (single fusion stage).

---

## 7. Index migration plan (decision 7)

1. **Additive migration** `migrations/tenant/007_code_symbols_fts.sql` (the GENERATED column + GIN index of §3.1). No data rewrite; GENERATED STORED populates synchronously on existing rows at `ALTER` time per Postgres semantics, and self-maintains thereafter.
2. **Zero-downtime**: dense path is untouched throughout. The sparse column exists but is unused until the flag flips.
3. **Per-tenant cutover criteria** (all must hold before `hybrid = on` for a tenant):
   a. migration 007 applied and `fts` populated for all of the tenant's repos;
   b. S6 eval clears the §8 regression threshold for that tenant's golden set (or the global set if none).
4. **Reversibility**: flag **off** ⇒ exact current dense-only behavior. Rollback = flip flag; the column/index can remain (inert) or be dropped by a later additive-down migration.

---

## 8. Eval harness contract (decision 5 — consumed by S6)

**Golden Q-A fixture schema** (checked into the repo, reuses the existing CI runner — framework choice is out of scope):

```jsonc
{
  "id": "q-001",
  "query": "where do we enforce the per-tenant session cap",
  "repo_id": "uuid",
  "relevant_chunks": ["sym:<fqn>", "…"],   // ground-truth symbol ids
  "notes": "optional"
}
```

**Metrics**: Recall@5, Recall@10, MRR, nDCG@10, citation-precision (fraction of returned citations whose `file_path`/`line_range` actually contain a ground-truth answer span).

**CI regression threshold** (blocking gate): nDCG@10 must not drop > **2%** vs the committed baseline, and Recall@10 must not drop at all. A drop fails CI and blocks the flag flip.

---

## 9. Latency & cost budgets (decision 6 — consumed by S7)

End-to-end (query received → ranked results serialized), warm caches:

| Mode | p50 | p95 | p99 |
|------|-----|-----|-----|
| Dense-only (flag off, baseline) | ≤ 120 ms | ≤ 300 ms | ≤ 500 ms |
| Hybrid (flag on, no rerank) | ≤ 180 ms | ≤ 400 ms | ≤ 700 ms |
| Hybrid + cross-encoder rerank (top-50) | ≤ 350 ms | ≤ 800 ms | ≤ 1200 ms |

**Per-tenant cost ceilings**: rerank candidate set capped at **50**; multi-query capped at **3**. LLM-rewrite and LLM-rerank are opt-in only and carry a configurable per-tenant token ceiling (default **disabled** ⇒ zero LLM cost). S7 enforces these as load-test assertions and emits `rb-metrics` counters/histograms per mode.

---

## 10. Tenant isolation (decision 8 — both sites)

- **Single entry point**: `rb_query::hybrid::hybrid_search(tenant: &TenantId, vector, query_text, opts) -> Result<Vec<HybridHit>, QueryError>`, mirroring `semantic_search`'s mandatory-tenant signature.
- **Dense leg**: `TenantVectorStore::search` `must`-filter (unchanged).
- **Sparse leg**: query runs against the tenant-qualified `code_symbols` schema (`TenantCtx::qualify`) — physically isolated.
- **Both `embed_query` sites migrate** to `hybrid_search` so neither `routes/query/search.rs` nor `routes/mcp/dispatch.rs` can bypass fusion or isolation.
- **Regression test (binding on S2)**: assert zero cross-tenant rows on the hybrid path at *both* sites — seed two tenants, query one, assert no rows from the other on dense **and** sparse legs.

---

## 11. Rejected alternatives

- **tantivy (sparse)** — new dependency + a separate index lifecycle to keep in sync with the projector; does not reuse per-tenant PG schema isolation. PG FTS reuses existing infra and isolation for free.
- **Qdrant sparse (BM42/SPLADE)** — requires a sparse-embedding model Ollama can't host well; adds a second model pipeline. Rejected for self-hosted simplicity.
- **Elasticsearch / OpenSearch** — heavy operational footprint, pushes toward managed-ish infra; violates the self-hosted-simple posture.
- **ParadeDB / `pg_search` (true BM25 in PG)** — closest to "real BM25" but adds a Postgres **extension** dependency (infra change). Deferred: revisit only if S6 shows native `ts_rank_cd` recall is insufficient. (Documented so the upgrade path is known.)
- **Weighted-sum fusion** — needs score normalization + per-tenant weight tuning; RRF is parameter-free and scale-robust.
- **Learned fusion / learned reranker (trained)** — no training data today; defer until the eval set is mature.
- **Separate reranker service** — premature; a network hop + deploy unit before we have evidence of CPU contention. In-process first; extract later if metrics justify.
- **LLM-rerank as default** — unbounded latency/cost; offered as per-tenant opt-in only.
- **Synonym-dictionary query expansion** — brittle for code identifiers; LLM rewrite (off by default) chosen instead.
- **Embedding-model upgrade in Wave 10** — orthogonal, separately reversible; out of scope here to keep the hybrid change isolated and measurable.

---

## 12. New dependency justification

**`fastembed` (Apache-2.0)** — one new crate, the *only* new external dependency this ADR authorizes. Hosts the local ONNX cross-encoder reranker (and could host sparse/dense models later) entirely in-process, satisfying the self-hosted constraint without a managed service or a new network service. License is on the allowed list (MIT/Apache-2.0/BSD/ISC). Gate-2 (`cargo deny`) will confirm the full transitive license set at S2 PR time. No other new external dependencies are authorized; sparse search, fusion, and query rewrite use Postgres and the existing Ollama runtime.

---

## 13. Consequences

- **Easier**: token-literal recall, citations for the chat UI, a measurable quality baseline with a CI guard, per-tenant tuning of cost/quality.
- **Harder**: one more ranking stage to reason about; a new crate (`rb-rerank`) and a new model artifact to ship; eval fixtures must be authored and maintained.
- **Reversible**: the entire feature collapses to today's dense-only behavior with one flag.

---

## 14. Downstream stream contracts (for CTO when opening S2–S7)

- **S2 (tracer-bullet, vertical slice)** — migration 007 + `rb_query::hybrid` (RRF over dense+FTS) + both `embed_query` sites on the new entry point + `CitationV1` in `rb-schemas`, behind the flag (default off). Delivers a demoable end-to-end hybrid result. **This is the required Wave-10 tracer slice.**
- **S3** — `rb-rerank` crate + in-process cross-encoder stage (flag-gated).
- **S4** — chat UI consumes `CitationV1` (this ADR owns the contract; S4 owns rendering).
- **S5** — query-understanding (multi-query rewrite, per-tenant disable hook).
- **S6** — eval harness + golden set + CI regression gate (§8).
- **S7** — latency/cost load tests + `rb-metrics` instrumentation + per-tenant ceilings (§9).

Blocker wiring: S3–S7 block on S2 (entry point + contract). S4 blocks on the `CitationV1` type landing in `rb-schemas` (part of S2).
