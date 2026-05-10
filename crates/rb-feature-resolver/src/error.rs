use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FeatureResolveError {
    #[error("failed to read manifest at {path}: {source}")]
    ManifestRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse manifest at {path}: {source}")]
    ManifestParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("unknown feature `{feature}` requested for crate `{crate_name}`")]
    UnknownFeature { crate_name: String, feature: String },

    #[error("cyclic feature dependency detected involving `{feature}`")]
    CyclicDependency { feature: String },
}
