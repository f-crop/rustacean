mod error;
mod manifest;
mod resolver;
mod types;

pub use error::FeatureResolveError;
pub use manifest::{
    CargoManifest, DependencySpec, DetailedDependency, PackageMetadata, WorkspaceSection,
};
pub use resolver::resolve;
pub use types::{FeatureSet, ResolvedFeatureSet};
