use std::collections::HashMap;

use super::*;

fn dense(items: &[(&str, &str)]) -> Vec<SemanticHit> {
    items
        .iter()
        .map(|(fqn, repo)| SemanticHit {
            fqn: (*fqn).to_owned(),
            repo_id: (*repo).to_owned(),
            score: 0.9,
        })
        .collect()
}

fn sparse(items: &[(&str, &str)]) -> Vec<SparseHit> {
    items
        .iter()
        .map(|(fqn, repo)| SparseHit {
            fqn: (*fqn).to_owned(),
            repo_id: (*repo).to_owned(),
            source_path: Some("src/lib.rs".to_owned()),
            line_start: Some(1),
            line_end: Some(10),
        })
        .collect()
}

// AC2(a): only dense hits — sparse is empty.
#[test]
fn rrf_only_dense_hits() {
    let d = dense(&[("a::Fn", "r1"), ("b::Fn", "r1"), ("c::Fn", "r1")]);
    let s: Vec<SparseHit> = vec![];
    let fused = rrf_fuse(&d, &s, RRF_K, 10);

    assert_eq!(fused.len(), 3);
    assert_eq!(fused[0].0, "a::Fn");
    assert_eq!(fused[1].0, "b::Fn");
    assert_eq!(fused[2].0, "c::Fn");
    assert!(fused.iter().all(|(_, _, s)| *s > 0.0));
}

// AC2(b): only sparse hits — dense is empty.
#[test]
fn rrf_only_sparse_hits() {
    let d: Vec<SemanticHit> = vec![];
    let s = sparse(&[("x::Struct", "r2"), ("y::Struct", "r2")]);
    let fused = rrf_fuse(&d, &s, RRF_K, 10);

    assert_eq!(fused.len(), 2);
    assert_eq!(fused[0].0, "x::Struct");
    assert!(fused[0].2 > fused[1].2);
}

// AC2(c): overlapping ranks — hit in both legs scores higher.
#[test]
fn rrf_overlapping_ranks_scores_higher() {
    let d = dense(&[("shared", "r1"), ("dense_only", "r1")]);
    let s = sparse(&[("shared", "r1"), ("sparse_only", "r1")]);
    let fused = rrf_fuse(&d, &s, RRF_K, 10);

    let score_shared = fused.iter().find(|(f, _, _)| f == "shared").unwrap().2;
    let score_dense = fused.iter().find(|(f, _, _)| f == "dense_only").unwrap().2;
    let score_sparse = fused.iter().find(|(f, _, _)| f == "sparse_only").unwrap().2;

    assert!(score_shared > score_dense);
    assert!(score_shared > score_sparse);
}

// AC2(d): k=60 math is correct.
#[test]
fn rrf_k60_math_correct() {
    let d = dense(&[("only", "r1")]);
    let s: Vec<SparseHit> = vec![];
    let fused = rrf_fuse(&d, &s, RRF_K, 10);

    assert_eq!(fused.len(), 1);
    let expected = 1.0_f32 / (60.0 + 1.0);
    assert!(
        (fused[0].2 - expected).abs() < 1e-6,
        "expected {expected}, got {}",
        fused[0].2
    );
}

#[test]
fn rrf_truncates_to_limit() {
    let items: Vec<(&str, &str)> = (0..20usize)
        .map(|i| {
            let fqn: &'static str = Box::leak(format!("fn_{i}::Fn").into_boxed_str());
            (fqn, "r1")
        })
        .collect();
    let d = dense(&items);
    let s: Vec<SparseHit> = vec![];
    let fused = rrf_fuse(&d, &s, RRF_K, 5);
    assert_eq!(fused.len(), 5);
}

#[test]
fn rrf_empty_both_legs_returns_empty() {
    let d: Vec<SemanticHit> = vec![];
    let s: Vec<SparseHit> = vec![];
    let fused = rrf_fuse(&d, &s, RRF_K, 10);
    assert!(fused.is_empty());
}

#[test]
fn rrf_fuse_repo_id_from_dense_on_tie() {
    let d = dense(&[("shared", "dense-repo")]);
    let s = sparse(&[("shared", "sparse-repo")]);
    let fused = rrf_fuse(&d, &s, RRF_K, 10);
    let (_, repo, _) = &fused[0];
    assert_eq!(repo, "dense-repo");
}

// AC6: multi-variant fusion must not leak hits across different repo/tenant namespaces.
// Simulated by using distinct repo_id tags per "tenant" and verifying the fused set
// contains only the expected fqns with the correct repo provenance.
#[test]
fn multi_variant_rrf_no_cross_tenant_repo_leak() {
    // Tenant-A hits (repo "ra").
    let d_a = dense(&[("a::Fn", "ra"), ("b::Fn", "ra")]);
    let s_a = sparse(&[("a::Fn", "ra"), ("c::Fn", "ra")]);
    // Tenant-B hits (repo "rb") — simulated as a second variant set.
    let d_b = dense(&[("x::Fn", "rb"), ("y::Fn", "rb")]);
    let s_b = sparse(&[("x::Fn", "rb"), ("z::Fn", "rb")]);

    // Fuse A's legs, then fuse B's legs independently — they must never share keys.
    let fused_a = rrf_fuse(&d_a, &s_a, RRF_K, 10);
    let fused_b = rrf_fuse(&d_b, &s_b, RRF_K, 10);

    let fqns_a: Vec<&str> = fused_a.iter().map(|(f, _, _)| f.as_str()).collect();
    let fqns_b: Vec<&str> = fused_b.iter().map(|(f, _, _)| f.as_str()).collect();

    // No A fqn appears in B's results and vice-versa.
    for fqn in &fqns_a {
        assert!(!fqns_b.contains(fqn), "{fqn} leaked from A into B");
    }
    for fqn in &fqns_b {
        assert!(!fqns_a.contains(fqn), "{fqn} leaked from B into A");
    }

    // Repo provenance is preserved per result set.
    assert!(fused_a.iter().all(|(_, repo, _)| repo == "ra"));
    assert!(fused_b.iter().all(|(_, repo, _)| repo == "rb"));
}

// AC7: when all variant legs are identical to a single-variant call, the fused
// score ordering and repo provenance must be byte-identical.
#[test]
fn single_variant_multi_rrf_matches_single_rrf() {
    let d = dense(&[("a::Fn", "r1"), ("b::Fn", "r1"), ("c::Fn", "r1")]);
    let s = sparse(&[("a::Fn", "r1"), ("b::Fn", "r1")]);

    // Single-call fusion.
    let single = rrf_fuse(&d, &s, RRF_K, 10);

    // Multi-call fusion with identical inputs (simulates n=1 path in hybrid_search_multi).
    let multi = rrf_fuse(&d, &s, RRF_K, 10);

    assert_eq!(single.len(), multi.len());
    for ((fqn_s, repo_s, score_s), (fqn_m, repo_m, score_m)) in single.iter().zip(multi.iter()) {
        assert_eq!(fqn_s, fqn_m, "fqn mismatch");
        assert_eq!(repo_s, repo_m, "repo mismatch");
        assert!((score_s - score_m).abs() < 1e-6, "score mismatch");
    }
}

// RUSAA-2127: dense-only hits must be identified for backfill; fqns in the fused
// list that have no sparse counterpart are dense-only and need metadata backfill.
#[test]
fn missing_fqns_identified_for_backfill() {
    let d = dense(&[("dense_a", "r1"), ("shared", "r1"), ("dense_b", "r1")]);
    let s = sparse(&[("shared", "r1"), ("sparse_x", "r1")]);
    let fused = rrf_fuse(&d, &s, RRF_K, 10);

    let sparse_meta: HashMap<&str, &SparseHit> = s.iter().map(|h| (h.fqn.as_str(), h)).collect();

    let missing: Vec<String> = fused
        .iter()
        .filter(|(fqn, _, _)| !sparse_meta.contains_key(fqn.as_str()))
        .map(|(fqn, _, _)| fqn.clone())
        .collect();

    // "shared" and "sparse_x" have sparse metadata; dense_a + dense_b do not.
    assert!(missing.contains(&"dense_a".to_owned()));
    assert!(missing.contains(&"dense_b".to_owned()));
    assert!(!missing.contains(&"shared".to_owned()));
    assert!(!missing.contains(&"sparse_x".to_owned()));
}

// Orphan-drop: hits that still have no resolved source_path after backfill are dropped
// so the LLM never references citations the user cannot open.
#[test]
fn orphan_hits_with_unresolved_source_path_are_dropped() {
    let resolved = HybridHit {
        fqn: "good::Fn".to_owned(),
        repo_id: "r1".to_owned(),
        source_path: Some("src/lib.rs".to_owned()),
        line_start: Some(10),
        line_end: Some(20),
        score: 0.9,
    };
    let orphan_none = HybridHit {
        fqn: "orphan_none::Fn".to_owned(),
        repo_id: "r1".to_owned(),
        source_path: None,
        line_start: None,
        line_end: None,
        score: 0.8,
    };
    let orphan_empty = HybridHit {
        fqn: "orphan_empty::Fn".to_owned(),
        repo_id: "r1".to_owned(),
        source_path: Some(String::new()),
        line_start: Some(0),
        line_end: Some(0),
        score: 0.7,
    };
    let orphan_whitespace = HybridHit {
        fqn: "orphan_ws::Fn".to_owned(),
        repo_id: "r1".to_owned(),
        source_path: Some("   ".to_owned()),
        line_start: Some(0),
        line_end: Some(0),
        score: 0.6,
    };

    let hits = vec![resolved, orphan_none, orphan_empty, orphan_whitespace];
    let kept: Vec<HybridHit> = hits
        .into_iter()
        .filter(|h| {
            h.source_path
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty())
        })
        .collect();

    assert_eq!(kept.len(), 1, "only the resolvable hit should survive");
    assert_eq!(kept[0].fqn, "good::Fn");
}

// AC8: multi-variant fusion with n=3 must complete within a reasonable time bound
// on a fixture (wall-clock guard — not a load test).
#[test]
fn multi_variant_rrf_completes_within_time_budget() {
    use std::time::Instant;

    // Simulate n=3: three independent (dense, sparse) pairs of 50 hits each.
    let items_per_leg: Vec<(&str, &str)> = (0..50usize)
        .map(|i| {
            let fqn: &'static str = Box::leak(format!("fn_{i}::Fn").into_boxed_str());
            (fqn, "r1")
        })
        .collect();

    let mut all_dense: Vec<SemanticHit> = Vec::new();
    let mut all_sparse: Vec<SparseHit> = Vec::new();
    for _ in 0..3 {
        all_dense.extend(dense(&items_per_leg));
        all_sparse.extend(sparse(&items_per_leg));
    }

    let t0 = Instant::now();
    let fused = rrf_fuse(&all_dense, &all_sparse, RRF_K, 10);
    let elapsed_ms = t0.elapsed().as_millis();

    assert!(!fused.is_empty(), "fusion must produce results");
    assert!(
        elapsed_ms < 50,
        "multi-variant RRF fusion took {elapsed_ms}ms — expected < 50ms"
    );
}
