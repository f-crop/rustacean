#!/usr/bin/env python3
"""Retrieval eval runner for hybrid search (ADR-014 §8, Wave 10 S6).

Modes:
  ci (default)   -- read pre-computed results-snapshot.json, evaluate metrics,
                     compare against baseline.json, exit 1 on regression.
  regenerate     -- call live hybrid_search API, write new snapshot + baseline.

Determinism: seed=42 is documented in results-snapshot.json; this script
has no RNG -- metric computation is fully deterministic.
"""

import argparse
import json
import math
import sys
import urllib.request
import urllib.error
from pathlib import Path
from typing import Any

NDCG_REGRESSION_THRESHOLD = 0.02
RECALL10_REGRESSION_THRESHOLD = 0.0
TOP_K_RECALL_LOW = 5
TOP_K_RECALL_HIGH = 10
TOP_K_NDCG = 10
TOP_K_CITATION_PRECISION = 10

DEFAULT_GOLDEN_DIR = "docs/retrieval-eval/golden"
DEFAULT_SNAPSHOT = "docs/retrieval-eval/results-snapshot.json"
DEFAULT_BASELINE = "docs/retrieval-eval/baseline.json"
SCHEMA_VERSION = "1"
SEED = 42


def load_golden_fixtures(golden_dir: Path) -> list[dict[str, Any]]:
    fixtures = []
    for path in sorted(golden_dir.glob("*.json")):
        data = json.loads(path.read_text())
        if isinstance(data, list):
            fixtures.extend(data)
        else:
            fixtures.append(data)
    if not fixtures:
        raise SystemExit(f"No golden fixtures found in {golden_dir}")
    return fixtures


def load_snapshot(snapshot_path: Path) -> dict[str, list[dict[str, Any]]]:
    data = json.loads(snapshot_path.read_text())
    return data.get("results", {})


def load_baseline(baseline_path: Path) -> dict[str, Any]:
    return json.loads(baseline_path.read_text())


def _parse_relevant(raw: list[str]) -> set[str]:
    out = set()
    for r in raw:
        fqn = r.removeprefix("sym:")
        out.add(fqn)
    return out


def _top_k_fqns(results: list[dict[str, Any]], k: int) -> list[str]:
    return [r["fqn"] for r in results[:k]]


def recall_at_k(results: list[dict[str, Any]], relevant: set[str], k: int) -> float:
    if not relevant:
        return 0.0
    top = set(_top_k_fqns(results, k))
    return len(top & relevant) / len(relevant)


def mrr(results: list[dict[str, Any]], relevant: set[str]) -> float:
    for rank, r in enumerate(results, start=1):
        if r["fqn"] in relevant:
            return 1.0 / rank
    return 0.0


def ndcg_at_k(results: list[dict[str, Any]], relevant: set[str], k: int) -> float:
    if not relevant:
        return 0.0
    dcg = 0.0
    for rank, r in enumerate(results[:k], start=1):
        if r["fqn"] in relevant:
            dcg += 1.0 / math.log2(rank + 1)
    idcg = sum(1.0 / math.log2(i + 2) for i in range(min(len(relevant), k)))
    if idcg == 0.0:
        return 0.0
    return dcg / idcg


def citation_precision_at_k(
    results: list[dict[str, Any]], relevant: set[str], k: int
) -> float:
    if k == 0:
        return 0.0
    top = _top_k_fqns(results, k)
    hits = sum(1 for fqn in top if fqn in relevant)
    return hits / k


def evaluate(
    fixtures: list[dict[str, Any]],
    snapshot: dict[str, list[dict[str, Any]]],
) -> dict[str, Any]:
    per_query = []
    n_missing = 0

    for fx in fixtures:
        qid = fx["id"]
        results = snapshot.get(qid)
        if results is None:
            n_missing += 1
            per_query.append({"id": qid, "missing": True})
            continue
        relevant = _parse_relevant(fx.get("relevant_chunks", []))
        row = {
            "id": qid,
            "query": fx["query"],
            "n_relevant": len(relevant),
            "recall_at_5": recall_at_k(results, relevant, TOP_K_RECALL_LOW),
            "recall_at_10": recall_at_k(results, relevant, TOP_K_RECALL_HIGH),
            "mrr": mrr(results, relevant),
            "ndcg_at_10": ndcg_at_k(results, relevant, TOP_K_NDCG),
            "citation_precision": citation_precision_at_k(
                results, relevant, TOP_K_CITATION_PRECISION
            ),
        }
        per_query.append(row)

    evaluated = [r for r in per_query if not r.get("missing")]
    n = len(evaluated)

    def avg(key: str) -> float:
        if n == 0:
            return 0.0
        return round(sum(r[key] for r in evaluated) / n, 6)

    return {
        "n_queries": len(fixtures),
        "n_evaluated": n,
        "n_missing": n_missing,
        "metrics": {
            "recall_at_5": avg("recall_at_5"),
            "recall_at_10": avg("recall_at_10"),
            "mrr": avg("mrr"),
            "ndcg_at_10": avg("ndcg_at_10"),
            "citation_precision": avg("citation_precision"),
        },
        "per_query": per_query,
    }


def check_regression(
    current: dict[str, Any], baseline: dict[str, Any]
) -> list[str]:
    failures = []
    bm = baseline.get("metrics", {})
    cm = current.get("metrics", {})

    ndcg_base = bm.get("ndcg_at_10", 0.0)
    ndcg_curr = cm.get("ndcg_at_10", 0.0)
    ndcg_drop = ndcg_base - ndcg_curr
    if ndcg_drop > NDCG_REGRESSION_THRESHOLD:
        failures.append(
            f"nDCG@10 dropped {ndcg_drop:.4f} "
            f"(baseline={ndcg_base:.6f}, current={ndcg_curr:.6f}, "
            f"threshold={NDCG_REGRESSION_THRESHOLD})"
        )

    r10_base = bm.get("recall_at_10", 0.0)
    r10_curr = cm.get("recall_at_10", 0.0)
    r10_drop = r10_base - r10_curr
    if r10_drop > RECALL10_REGRESSION_THRESHOLD:
        failures.append(
            f"Recall@10 dropped {r10_drop:.4f} "
            f"(baseline={r10_base:.6f}, current={r10_curr:.6f})"
        )

    return failures


def _metric_row(name: str, baseline_val: float, current_val: float) -> str:
    delta = current_val - baseline_val
    sign = "+" if delta >= 0 else ""
    flag = " ✓" if delta >= 0 else " ✗"
    return (
        f"  {name:<22} baseline={baseline_val:.6f}  "
        f"current={current_val:.6f}  delta={sign}{delta:.6f}{flag}"
    )


def print_diff_report(
    current_result: dict[str, Any],
    baseline: dict[str, Any],
    failures: list[str],
) -> None:
    bm = baseline.get("metrics", {})
    cm = current_result.get("metrics", {})

    print("\n=== Retrieval Eval Report ===")
    print(
        f"Queries: {current_result['n_evaluated']}/{current_result['n_queries']} evaluated"
        + (
            f", {current_result['n_missing']} missing from snapshot"
            if current_result["n_missing"]
            else ""
        )
    )
    print()
    print("Metrics vs baseline:")
    for key in ("recall_at_5", "recall_at_10", "mrr", "ndcg_at_10", "citation_precision"):
        print(_metric_row(key, bm.get(key, 0.0), cm.get(key, 0.0)))

    if failures:
        print("\nREGRESSION FAILURES:")
        for f in failures:
            print(f"  • {f}")
            print(f"::error::{f}")
    else:
        print("\nAll thresholds passed.")
    print()


def _call_api(
    api_url: str, api_key: str, repo_id: str, query: str, limit: int
) -> list[dict[str, Any]]:
    url = f"{api_url.rstrip('/')}/api/search"
    payload = json.dumps(
        {"query": query, "repo_id": repo_id, "limit": limit}
    ).encode()
    req = urllib.request.Request(
        url,
        data=payload,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {api_key}",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            body = json.loads(resp.read().decode())
    except urllib.error.HTTPError as e:
        raise SystemExit(f"API error {e.code} for query: {query}") from e
    results = body.get("results", [])
    return [
        {
            "fqn": r.get("fqn", ""),
            "file_path": r.get("file_path", ""),
            "line_start": r.get("line_start", 0),
            "line_end": r.get("line_end", 0),
            "score": r.get("score", 0.0),
        }
        for r in results
    ]


def run_ci(args: argparse.Namespace) -> int:
    golden_dir = Path(args.golden_dir)
    snapshot_path = Path(args.snapshot)
    baseline_path = Path(args.baseline)

    for p in (golden_dir, snapshot_path, baseline_path):
        if not p.exists():
            print(f"::error::Required path not found: {p}")
            return 1

    fixtures = load_golden_fixtures(golden_dir)
    snapshot = load_snapshot(snapshot_path)
    baseline = load_baseline(baseline_path)

    result = evaluate(fixtures, snapshot)
    failures = check_regression(result, baseline)
    print_diff_report(result, baseline, failures)
    return 1 if failures else 0


def run_regenerate(args: argparse.Namespace) -> int:
    api_url = args.api_url
    api_key = args.api_key
    if not api_url or not api_key:
        print("--api-url and --api-key are required for regenerate mode", file=sys.stderr)
        return 1

    golden_dir = Path(args.golden_dir)
    snapshot_path = Path(args.snapshot)
    baseline_path = Path(args.baseline)

    fixtures = load_golden_fixtures(golden_dir)
    results: dict[str, list[dict[str, Any]]] = {}
    for fx in fixtures:
        print(f"  querying {fx['id']}: {fx['query'][:60]}...")
        hits = _call_api(
            api_url,
            api_key,
            fx["repo_id"],
            fx["query"],
            limit=TOP_K_NDCG,
        )
        results[fx["id"]] = hits

    snapshot = {
        "schema_version": SCHEMA_VERSION,
        "seed": SEED,
        "generated_at": args.generated_at,
        "commit_sha": args.commit_sha or "",
        "description": (
            "Pre-computed hybrid_search results for golden Q-A set (flag-on path). "
            "Regenerate via the regenerate-eval-baseline workflow."
        ),
        "results": results,
    }
    snapshot_path.write_text(json.dumps(snapshot, indent=2) + "\n")
    print(f"Wrote {snapshot_path}")

    eval_result = evaluate(fixtures, results)
    baseline = {
        "schema_version": SCHEMA_VERSION,
        "generated_at": args.generated_at,
        "commit_sha": args.commit_sha or "",
        "seed": SEED,
        "n_queries": eval_result["n_queries"],
        "n_evaluated": eval_result["n_evaluated"],
        "n_missing": eval_result["n_missing"],
        "metrics": eval_result["metrics"],
        "thresholds": {
            "ndcg_at_10_max_drop": NDCG_REGRESSION_THRESHOLD,
            "recall_at_10_max_drop": RECALL10_REGRESSION_THRESHOLD,
        },
    }
    baseline_path.write_text(json.dumps(baseline, indent=2) + "\n")
    print(f"Wrote {baseline_path}")
    print("\nNew metrics:")
    for k, v in eval_result["metrics"].items():
        print(f"  {k}: {v}")
    return 0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument(
        "--mode",
        choices=["ci", "regenerate"],
        default="ci",
        help="ci: evaluate against snapshot; regenerate: call live API",
    )
    p.add_argument("--golden-dir", default=DEFAULT_GOLDEN_DIR)
    p.add_argument("--snapshot", default=DEFAULT_SNAPSHOT)
    p.add_argument("--baseline", default=DEFAULT_BASELINE)
    p.add_argument("--api-url", default="", help="Base URL for regenerate mode")
    p.add_argument("--api-key", default="", help="Bearer token for regenerate mode")
    p.add_argument(
        "--commit-sha",
        default="",
        help="Git SHA to embed in regenerated files",
    )
    p.add_argument(
        "--generated-at",
        default="1970-01-01T00:00:00Z",
        help="ISO timestamp to embed in regenerated files",
    )
    return p.parse_args()


def main() -> None:
    args = parse_args()
    if args.mode == "regenerate":
        sys.exit(run_regenerate(args))
    else:
        sys.exit(run_ci(args))


if __name__ == "__main__":
    main()
