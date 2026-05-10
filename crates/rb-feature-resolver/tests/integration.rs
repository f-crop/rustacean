use std::path::Path;

use rb_feature_resolver::{CargoManifest, FeatureSet, resolve};

fn fixtures_dir() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/manifests"
    ))
}

fn load(name: &str) -> CargoManifest {
    CargoManifest::from_path(&fixtures_dir().join(name)).expect("fixture must parse")
}

// ── basic.toml ────────────────────────────────────────────────────────────────

#[test]
fn basic_default_features() {
    let manifest = load("basic.toml");
    let req = FeatureSet::default();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(
        resolved.features().contains("default"),
        "default must be present"
    );
    assert!(
        resolved.features().contains("logging"),
        "default enables logging"
    );
    assert!(
        !resolved.features().contains("tracing"),
        "tracing not in default"
    );
    assert!(
        !resolved.features().contains("metrics"),
        "metrics not in default"
    );
}

#[test]
fn basic_explicit_feature() {
    let manifest = load("basic.toml");
    let req = FeatureSet::with_features(["tracing"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.features().contains("tracing"));
    // tracing enables logging
    assert!(resolved.features().contains("logging"));
    assert!(!resolved.features().contains("default"));
    assert!(!resolved.features().contains("metrics"));
}

#[test]
fn basic_full_feature_expands_all() {
    let manifest = load("basic.toml");
    let req = FeatureSet::with_features(["full"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    for f in ["full", "logging", "tracing", "metrics"] {
        assert!(resolved.features().contains(f), "{f} should be resolved");
    }
}

#[test]
fn basic_no_default_features_empty_request() {
    let manifest = load("basic.toml");
    let req = FeatureSet::default().no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.is_empty(), "nothing should be resolved");
}

// ── no_defaults.toml ──────────────────────────────────────────────────────────

#[test]
fn no_defaults_manifest_uses_default_features_is_noop() {
    // Manifest has no `default` feature; requesting with default features still gives nothing.
    let manifest = load("no_defaults.toml");
    let req = FeatureSet::default(); // uses_default_features = true
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();
    assert!(resolved.is_empty());
}

#[test]
fn no_defaults_explicit_full() {
    let manifest = load("no_defaults.toml");
    let req = FeatureSet::with_features(["full"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.features().contains("full"));
    assert!(resolved.features().contains("serde-support"));
    assert!(resolved.features().contains("async-support"));
}

// ── with_optional_deps.toml ───────────────────────────────────────────────────

#[test]
fn optional_dep_activation_not_in_feature_set() {
    // `json = ["dep:serde_json"]` — the dep: spec should not appear as a feature name.
    let manifest = load("with_optional_deps.toml");
    let req = FeatureSet::default(); // enables "json" via default
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.features().contains("json"));
    assert!(
        !resolved.features().contains("dep:serde_json"),
        "dep: refs must not leak"
    );
}

#[test]
fn cross_crate_feature_ref_not_in_feature_set() {
    // `derive = ["serde/derive"]` — cross-crate ref should not appear as a feature name.
    let manifest = load("with_optional_deps.toml");
    let req = FeatureSet::with_features(["derive"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.features().contains("derive"));
    assert!(
        !resolved.features().contains("serde/derive"),
        "dep/feature refs must not leak"
    );
}

#[test]
fn full_feature_with_optional_deps() {
    let manifest = load("with_optional_deps.toml");
    let req = FeatureSet::with_features(["full"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();

    assert!(resolved.features().contains("full"));
    assert!(resolved.features().contains("json"));
    assert!(resolved.features().contains("xml"));
    assert!(resolved.features().contains("derive"));
}

// ── round-trip: cargo_args ────────────────────────────────────────────────────

#[test]
fn cargo_args_are_sorted() {
    let manifest = load("basic.toml");
    let req = FeatureSet::with_features(["full"]).no_default_features();
    let resolved = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap();
    let args = resolved.as_cargo_args();

    let mut sorted = args.clone();
    sorted.sort_unstable();
    assert_eq!(
        args, sorted,
        "as_cargo_args must return sorted feature names"
    );
}

// ── unknown feature error ─────────────────────────────────────────────────────

#[test]
fn unknown_feature_returns_error() {
    let manifest = load("basic.toml");
    let req = FeatureSet::with_features(["does-not-exist"]).no_default_features();
    let err = resolve(Path::new("/nonexistent"), &manifest, &req).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist"),
        "error must name the unknown feature"
    );
}
