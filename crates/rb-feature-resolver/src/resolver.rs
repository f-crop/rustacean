use std::collections::{BTreeSet, VecDeque};
use std::path::Path;

use crate::{
    error::FeatureResolveError,
    manifest::{CargoManifest, DependencySpec},
    types::{FeatureSet, ResolvedFeatureSet},
};

/// Resolve `requested` against `manifest`'s `[features]` table, producing the canonical feature
/// set to pass to `cargo expand --features`.
///
/// # Algorithm
///
/// 1. If `requested.uses_default_features()` and the manifest declares a `default` feature, seed
///    the BFS queue with `"default"` and all of its direct children.
/// 2. Add every explicitly requested feature to the queue.
/// 3. BFS-expand: each feature name is looked up in `manifest.features`; any entry that is a
///    plain feature name (not a `dep:…` activation or a `dep/feature` cross-crate reference) is
///    enqueued.
/// 4. Collect workspace-unified features: check every other workspace member's dependencies for
///    entries that name this crate with additional features, and include those too.
///
/// # Errors
///
/// Returns [`FeatureResolveError::UnknownFeature`] if a feature in `requested` does not appear in
/// `manifest.features` (and is not `"default"`).
///
/// Returns [`FeatureResolveError::ManifestRead`] / [`FeatureResolveError::ManifestParse`] if any
/// workspace member manifest cannot be read.
pub fn resolve(
    workspace_root: &Path,
    manifest: &CargoManifest,
    requested: &FeatureSet,
) -> Result<ResolvedFeatureSet, FeatureResolveError> {
    // Guard against malformed manifests with circular feature deps.
    const MAX_ITERATIONS: usize = 4096;

    let mut pending: VecDeque<String> = VecDeque::new();

    // Validate requested features exist before we begin.
    let crate_name = manifest.package_name().unwrap_or("<unknown>");
    for f in requested.features() {
        if !manifest.features.contains_key(f.as_str()) {
            return Err(FeatureResolveError::UnknownFeature {
                crate_name: crate_name.to_owned(),
                feature: f.clone(),
            });
        }
        pending.push_back(f.clone());
    }

    // Seed defaults unless opted out.
    if requested.uses_default_features() {
        if let Some(default_deps) = manifest.features.get("default") {
            // Enqueue "default" itself so it appears in the resolved set.
            pending.push_back("default".to_owned());
            for spec in default_deps {
                if let Some(name) = as_self_feature(spec) {
                    if manifest.features.contains_key(name) {
                        pending.push_back(name.to_owned());
                    }
                }
            }
        }
    }

    // Collect workspace-unified features (other members that depend on this crate).
    collect_workspace_unified(&mut pending, workspace_root, manifest)?;

    // BFS expansion.
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    let mut iterations = 0usize;

    while let Some(feature) = pending.pop_front() {
        if resolved.contains(&feature) {
            continue;
        }
        iterations += 1;
        if iterations > MAX_ITERATIONS {
            return Err(FeatureResolveError::CyclicDependency {
                feature: feature.clone(),
            });
        }
        resolved.insert(feature.clone());

        if let Some(deps) = manifest.features.get(&feature) {
            for spec in deps {
                if let Some(name) = as_self_feature(spec) {
                    if manifest.features.contains_key(name) && !resolved.contains(name) {
                        pending.push_back(name.to_owned());
                    }
                }
            }
        }
    }

    Ok(ResolvedFeatureSet::new(resolved))
}

/// Extract a self-feature name from a feature specification string.
///
/// Cargo feature specs have three forms:
/// - `"feature_name"` — enables another feature on the same crate → return `Some("feature_name")`
/// - `"dep:optional_dep"` — activates an optional dependency → return `None` (not a feature name)
/// - `"dep_name/feature"` — enables a feature on a dependency → return `None` (cross-crate ref)
fn as_self_feature(spec: &str) -> Option<&str> {
    if spec.starts_with("dep:") || spec.contains('/') {
        None
    } else {
        Some(spec)
    }
}

/// Walk workspace members and collect any features they request from the current crate.
///
/// This implements the feature-unification rule: if sibling crate B depends on this crate A with
/// `features = ["extra"]`, then `extra` is included in A's resolved set when A is compiled as
/// part of the workspace.
fn collect_workspace_unified(
    pending: &mut VecDeque<String>,
    workspace_root: &Path,
    manifest: &CargoManifest,
) -> Result<(), FeatureResolveError> {
    let ws_manifest_path = workspace_root.join("Cargo.toml");
    if !ws_manifest_path.exists() {
        return Ok(());
    }

    let ws_manifest = CargoManifest::from_path(&ws_manifest_path)?;
    let Some(workspace) = &ws_manifest.workspace else {
        return Ok(());
    };

    let Some(current_name) = manifest.package_name() else {
        return Ok(());
    };

    for member_glob in &workspace.members {
        let member_dir = workspace_root.join(member_glob);
        if !member_dir.is_dir() {
            continue;
        }
        let member_manifest_path = member_dir.join("Cargo.toml");
        if !member_manifest_path.exists() {
            continue;
        }
        // Skip members we cannot parse — they may be virtual or broken.
        let Ok(member) = CargoManifest::from_path(&member_manifest_path) else {
            continue;
        };

        // Skip ourselves.
        if member.package_name() == Some(current_name) {
            continue;
        }

        // Find any dependency entry for this crate in the member.
        for (dep_name, dep_spec) in &member.dependencies {
            if dep_name.as_str() == current_name {
                if let DependencySpec::Detailed(detail) = dep_spec {
                    for f in &detail.features {
                        if manifest.features.contains_key(f.as_str()) {
                            pending.push_back(f.clone());
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::manifest::{CargoManifest, PackageMetadata};

    fn manifest_with_features(name: &str, features: &[(&str, &[&str])]) -> CargoManifest {
        let mut f: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (k, vs) in features {
            f.insert(
                (*k).to_owned(),
                vs.iter().map(|s| (*s).to_owned()).collect(),
            );
        }
        CargoManifest {
            package: Some(PackageMetadata {
                name: name.to_owned(),
                version: None,
            }),
            features: f,
            ..Default::default()
        }
    }

    #[test]
    fn empty_request_no_defaults() {
        let m = manifest_with_features("my-crate", &[]);
        let req = FeatureSet::default().no_default_features();
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn default_feature_expanded() {
        let m = manifest_with_features(
            "my-crate",
            &[("default", &["foo"]), ("foo", &["bar"]), ("bar", &[])],
        );
        let req = FeatureSet::default(); // uses_default_features = true
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        let features: Vec<_> = result.iter().collect();
        assert!(features.contains(&"default"));
        assert!(features.contains(&"foo"));
        assert!(features.contains(&"bar"));
    }

    #[test]
    fn no_default_features_flag_respected() {
        let m = manifest_with_features(
            "my-crate",
            &[("default", &["foo"]), ("foo", &[]), ("extra", &[])],
        );
        let req = FeatureSet::with_features(["extra"]).no_default_features();
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        assert!(result.features().contains("extra"));
        assert!(!result.features().contains("foo"));
        assert!(!result.features().contains("default"));
    }

    #[test]
    fn dep_colon_spec_ignored() {
        // "dep:optional_dep" should not be treated as a feature name.
        let m = manifest_with_features(
            "my-crate",
            &[("default", &["dep:optional_dep"]), ("real_feature", &[])],
        );
        let req = FeatureSet::default();
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        assert!(result.features().contains("default"));
        assert!(!result.features().contains("dep:optional_dep"));
    }

    #[test]
    fn dep_slash_spec_ignored() {
        let m = manifest_with_features(
            "my-crate",
            &[("default", &["some_dep/derive"]), ("real", &[])],
        );
        let req = FeatureSet::default();
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        assert!(result.features().contains("default"));
        assert!(!result.features().contains("some_dep/derive"));
    }

    #[test]
    fn unknown_feature_errors() {
        let m = manifest_with_features("my-crate", &[("foo", &[])]);
        let req = FeatureSet::with_features(["nonexistent"]);
        let err = resolve(Path::new("/nonexistent"), &m, &req).unwrap_err();
        assert!(matches!(err, FeatureResolveError::UnknownFeature { .. }));
    }

    #[test]
    fn as_cargo_args_sorted() {
        let m = manifest_with_features(
            "my-crate",
            &[("default", &["b"]), ("a", &[]), ("b", &["a"])],
        );
        let req = FeatureSet::default();
        let result = resolve(Path::new("/nonexistent"), &m, &req).unwrap();
        let args = result.as_cargo_args();
        // BTreeSet guarantees alphabetical order.
        assert_eq!(args, vec!["a", "b", "default"]);
    }
}
