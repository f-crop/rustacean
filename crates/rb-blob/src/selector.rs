//! Factory: creates a `BlobStore` from `RB_BLOB_STORE` environment variable.
//!
//! Supported values:
//! - `filesystem` (default) — uses [`crate::filesystem::FilesystemStore`]
//! - `s3` — uses [`crate::s3::S3Store`] (requires `s3` feature)

use std::sync::Arc;

use crate::{BlobError, filesystem::FilesystemStore, store::BlobStore};

/// Build a [`BlobStore`] from `RB_BLOB_STORE` (default: `filesystem`).
///
/// # Errors
///
/// Returns [`BlobError::UnknownBackend`] for unrecognised values, or
/// backend-specific errors during initialisation.
pub async fn store_from_env() -> Result<Arc<dyn BlobStore>, BlobError> {
    let backend = std::env::var("RB_BLOB_STORE").unwrap_or_else(|_| "filesystem".to_string());

    match backend.as_str() {
        "filesystem" => Ok(Arc::new(FilesystemStore::from_env()?)),
        "s3" => {
            #[cfg(feature = "s3")]
            {
                let store = crate::s3::S3Store::from_env().await?;
                Ok(Arc::new(store))
            }
            #[cfg(not(feature = "s3"))]
            {
                Err(BlobError::UnknownBackend(
                    "s3 backend requested but the 's3' feature is not enabled".to_string(),
                ))
            }
        }
        other => Err(BlobError::UnknownBackend(other.to_string())),
    }
}
