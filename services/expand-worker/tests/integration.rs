//! Integration tests for `expand-worker`.
//!
//! These tests exercise the workspace-discovery and feature-resolution logic
//! against the fixture workspace under `tests/fixtures/feature-flagged-workspace/`.
//! `cargo expand` itself is NOT invoked here — that would require the binary and a
//! full Cargo build, which is unsuitable for unit-level CI. The integration test
//! validates the pipeline plumbing (discovery, manifest parsing, feature resolution)
//! against the fixture.

use std::path::PathBuf;

fn fixture_workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/feature-flagged-workspace")
}

#[test]
fn discovers_both_workspace_members() {
    let root = fixture_workspace();
    let root_toml = root.join("Cargo.toml");
    assert!(
        root_toml.exists(),
        "fixture workspace root Cargo.toml must exist"
    );

    let text = std::fs::read_to_string(&root_toml).unwrap();
    let parsed: toml::Value = toml::from_str(&text).unwrap();
    let members = parsed["workspace"]["members"].as_array().unwrap();
    assert_eq!(
        members.len(),
        2,
        "fixture must declare two workspace members"
    );
}

#[test]
fn alpha_feature_resolution_includes_logging_by_default() {
    let root = fixture_workspace();
    let manifest_path = root.join("crates/alpha/Cargo.toml");

    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: rb_feature_resolver::CargoManifest = toml::from_str(&text).unwrap();

    let requested = rb_feature_resolver::FeatureSet::default();
    let resolved = rb_feature_resolver::resolve(&root, &manifest, &requested).unwrap();

    let features: Vec<_> = resolved.features().iter().cloned().collect();
    assert!(
        features.contains(&"logging".to_string()),
        "default feature set must resolve to include 'logging'; got: {features:?}"
    );
    assert!(
        features.contains(&"default".to_string()),
        "resolved set must include 'default'; got: {features:?}"
    );
}

#[test]
fn beta_feature_resolution_no_non_default_features() {
    let root = fixture_workspace();
    let manifest_path = root.join("crates/beta/Cargo.toml");

    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: rb_feature_resolver::CargoManifest = toml::from_str(&text).unwrap();

    let requested = rb_feature_resolver::FeatureSet::default();
    let resolved = rb_feature_resolver::resolve(&root, &manifest, &requested).unwrap();

    let features: Vec<_> = resolved.features().iter().cloned().collect();
    // beta declares `default = []` (empty), so "default" may appear but "extra" must not.
    assert!(
        !features.contains(&"extra".to_string()),
        "beta default must not include 'extra'; got: {features:?}"
    );
}

#[test]
fn beta_feature_resolution_respects_no_default_features() {
    let root = fixture_workspace();
    let manifest_path = root.join("crates/alpha/Cargo.toml");

    let text = std::fs::read_to_string(&manifest_path).unwrap();
    let manifest: rb_feature_resolver::CargoManifest = toml::from_str(&text).unwrap();

    let requested = rb_feature_resolver::FeatureSet::default().no_default_features();
    let resolved = rb_feature_resolver::resolve(&root, &manifest, &requested).unwrap();

    let features: Vec<_> = resolved.features().iter().cloned().collect();
    assert!(
        !features.contains(&"logging".to_string()),
        "no_default_features must suppress 'logging'; got: {features:?}"
    );
}
