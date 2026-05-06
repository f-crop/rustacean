#!/usr/bin/env python3
"""UAT Fingerprint diff utility (RUSAA-696 — Pillar C Phase 2).

Compares two UAT fingerprint JSON files and reports drift:
  - Image SHA changes (which services changed, old vs new digest)
  - DB row count deltas (items / calls / relations)
  - Kafka topic offset deltas (how many new messages per topic)
  - Trace ID set changes (new vs removed traces)

Exit codes:
  0 — fingerprints are identical (no drift)
  1 — fingerprints differ (drift detected) OR one/both are invalid
  2 — usage / file error

Usage:
  python3 scripts/fingerprint-diff.py <baseline.json> <candidate.json>

  baseline  — previous PASS fingerprint (or main HEAD fingerprint)
  candidate — new fingerprint to compare against baseline

Example:
  # Compare current run against main HEAD fingerprint
  python3 scripts/fingerprint-diff.py /tmp/main-fingerprint.json /tmp/uat-fingerprint.json

  # Compare two run outputs side by side
  python3 scripts/fingerprint-diff.py runs/2026-05-06.json runs/2026-05-07.json
"""

import json
import sys
from pathlib import Path
from typing import Any


def load(path: str) -> dict[str, Any]:
    p = Path(path)
    if not p.exists():
        print(f"ERROR: file not found: {path}", file=sys.stderr)
        sys.exit(2)
    with p.open() as f:
        return json.load(f)


def compare_image_shas(
    baseline: dict[str, str], candidate: dict[str, str]
) -> list[str]:
    lines = []
    all_services = sorted(set(baseline) | set(candidate))
    for svc in all_services:
        b = baseline.get(svc, "<absent>")
        c = candidate.get(svc, "<absent>")
        if b != c:
            lines.append(f"  [{svc}]")
            lines.append(f"    - baseline:  {b}")
            lines.append(f"    + candidate: {c}")
    return lines


def compare_db(baseline: dict[str, int], candidate: dict[str, int]) -> list[str]:
    lines = []
    all_keys = sorted(set(baseline) | set(candidate))
    for key in all_keys:
        b = baseline.get(key, 0)
        c = candidate.get(key, 0)
        if b != c:
            delta = c - b
            sign = "+" if delta >= 0 else ""
            lines.append(f"  {key}: {b} → {c}  ({sign}{delta})")
    return lines


def compare_sse_offsets(
    baseline: dict[str, int], candidate: dict[str, int]
) -> list[str]:
    lines = []
    all_topics = sorted(set(baseline) | set(candidate))
    for topic in all_topics:
        b = baseline.get(topic, 0)
        c = candidate.get(topic, 0)
        if b != c:
            delta = c - b
            sign = "+" if delta >= 0 else ""
            lines.append(f"  {topic}: {b} → {c}  ({sign}{delta} messages)")
    return lines


def compare_trace_ids(
    baseline: list[str], candidate: list[str]
) -> list[str]:
    lines = []
    b_set = set(baseline)
    c_set = set(candidate)
    added = sorted(c_set - b_set)
    removed = sorted(b_set - c_set)
    for tid in added:
        lines.append(f"  + {tid}")
    for tid in removed:
        lines.append(f"  - {tid}")
    return lines


def main() -> None:
    if len(sys.argv) != 3:
        print(__doc__)
        sys.exit(2)

    baseline_path, candidate_path = sys.argv[1], sys.argv[2]
    baseline = load(baseline_path)
    candidate = load(candidate_path)

    print(f"Baseline:  {baseline_path}  (collected_at: {baseline.get('collected_at', '?')})")
    print(f"Candidate: {candidate_path}  (collected_at: {candidate.get('collected_at', '?')})")
    print()

    has_drift = False

    # Verdict validity check
    b_verdict = baseline.get("verdict", "unknown")
    c_verdict = candidate.get("verdict", "unknown")
    if b_verdict != "pass":
        print(f"WARNING: baseline verdict is '{b_verdict}' (not 'pass') — comparison may be misleading")
    if c_verdict != "pass":
        print(f"WARNING: candidate verdict is '{c_verdict}' (not 'pass') — fingerprint is invalid")
        has_drift = True

    # Image SHAs
    sha_diffs = compare_image_shas(
        baseline.get("image_shas", {}), candidate.get("image_shas", {})
    )
    if sha_diffs:
        has_drift = True
        print("IMAGE_SHAS: DRIFT DETECTED")
        for line in sha_diffs:
            print(line)
    else:
        print("IMAGE_SHAS: identical")
    print()

    # DB counts
    db_diffs = compare_db(baseline.get("db", {}), candidate.get("db", {}))
    if db_diffs:
        has_drift = True
        print("DB COUNTS: changed")
        for line in db_diffs:
            print(line)
    else:
        print("DB COUNTS: identical")
    print()

    # SSE / Kafka offsets
    sse_diffs = compare_sse_offsets(
        baseline.get("sse_offsets", {}), candidate.get("sse_offsets", {})
    )
    if sse_diffs:
        has_drift = True
        print("SSE_OFFSETS: changed (new messages since baseline)")
        for line in sse_diffs:
            print(line)
    else:
        print("SSE_OFFSETS: identical")
    print()

    # Trace IDs
    trace_diffs = compare_trace_ids(
        baseline.get("trace_ids", []), candidate.get("trace_ids", [])
    )
    if trace_diffs:
        has_drift = True
        print("TRACE_IDS: changed")
        for line in trace_diffs:
            print(line)
    else:
        print("TRACE_IDS: identical")
    print()

    if has_drift:
        print("RESULT: DRIFT DETECTED — fingerprints differ")
        sys.exit(1)
    else:
        print("RESULT: NO DRIFT — fingerprints are identical")
        sys.exit(0)


if __name__ == "__main__":
    main()
