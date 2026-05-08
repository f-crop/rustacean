//! S3-backed blob store. Requires the `s3` feature.
//!
//! ## Object layout
//!
//! ```text
//! <tenant_id>/<sha256>                      ← actual blob content
//! _manifest/<sha256>/<tenant_id>            ← empty ownership marker (per-tenant)
//! ```
//!
//! ## Tenant-isolation invariant
//!
//! The manifest marker is written **BEFORE** the blob object (B-HIGH-2).
//! This ensures:
//! - Orphaned manifest (manifest exists, blob missing)  → observable as `NotFound`, NOT a security breach.
//! - Orphaned blob    (blob exists, manifest missing)   → would be a breach; prevented by writing
//!   manifest first and aborting on manifest failure.
//!
//! ## Cross-tenant detection (B-HIGH-1)
//!
//! Each tenant gets its own marker at `_manifest/<sha256>/<tenant_id>`.
//! On a GET miss, we `list_objects_v2` under `_manifest/<sha256>/` to see if
//! ANY other tenant has a marker for this sha256.  If so → `TenantMismatch`.
//!
//! `delete` removes both the blob object AND the per-tenant marker so the
//! marker does not outlive the blob for that tenant.

use async_trait::async_trait;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tracing::instrument;

use crate::{BlobError, BlobRef, error::S3ErrorKind, store::BlobStore};

/// S3-backed blob store.
///
/// Configure via:
/// - `RB_BLOB_S3_BUCKET` — bucket name (default: `rb-blobs`)
/// - `RB_BLOB_S3_ENDPOINT` — override endpoint URL (e.g., for localstack)
/// - `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION` — standard AWS env vars
pub struct S3Store {
    client: aws_sdk_s3::Client,
    bucket: String,
}

/// Classify an SDK error into a typed [`S3ErrorKind`].
///
/// We inspect the HTTP status code first (most reliable), then fall back to
/// the stringified error for cases where the SDK does not surface a code.
fn classify_sdk_err<E: std::fmt::Display>(err: &SdkError<E>) -> S3ErrorKind {
    // Try to extract the HTTP status code from the raw response.
    let status = match err {
        SdkError::ServiceError(se) => se.raw().status().as_u16(),
        SdkError::ResponseError(re) => re.raw().status().as_u16(),
        _ => 0,
    };
    match status {
        404 => S3ErrorKind::NotFound,
        401 | 403 => S3ErrorKind::Auth,
        429 | 503 => S3ErrorKind::Throttled,
        0 => {
            // No HTTP response — likely a network/dispatch error.
            match err {
                SdkError::DispatchFailure(_) | SdkError::TimeoutError(_) => S3ErrorKind::Network,
                _ => S3ErrorKind::Other(err.to_string()),
            }
        }
        _ => S3ErrorKind::Other(err.to_string()),
    }
}

impl S3Store {
    /// Build from environment variables and validate bucket accessibility.
    ///
    /// Calls `HeadBucket` after creating the client so that misconfiguration
    /// is detected at startup rather than on the first hot-path operation
    /// (B-MED-5).
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::S3`] if the AWS config cannot be loaded or the
    /// bucket is not accessible.
    pub async fn from_env() -> Result<Self, BlobError> {
        let bucket = std::env::var("RB_BLOB_S3_BUCKET").unwrap_or_else(|_| "rb-blobs".to_string());

        let sdk_config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let mut s3_builder = aws_sdk_s3::config::Builder::from(&sdk_config);

        if let Ok(endpoint) = std::env::var("RB_BLOB_S3_ENDPOINT") {
            s3_builder = s3_builder.endpoint_url(endpoint).force_path_style(true);
        }

        let client = aws_sdk_s3::Client::from_conf(s3_builder.build());

        // Probe bucket accessibility at startup (B-MED-5).
        // Surfaces as BlobError::Configuration so callers see a boot-time error,
        // not a hot-path S3 error.
        client
            .head_bucket()
            .bucket(&bucket)
            .send()
            .await
            .map_err(|e| {
                BlobError::Configuration(format!(
                    "S3 bucket '{}' not accessible at startup: {}",
                    bucket,
                    classify_sdk_err(&e)
                ))
            })?;

        Ok(Self { client, bucket })
    }

    fn object_key(blob_ref: &BlobRef) -> String {
        format!("{}/{}", blob_ref.tenant_id, blob_ref.sha256)
    }

    /// Per-tenant manifest marker key (B-HIGH-1).
    ///
    /// Layout: `_manifest/<sha256>/<tenant_id>` — an empty object.
    fn manifest_key(sha256: &str, tenant_id: &uuid::Uuid) -> String {
        format!("_manifest/{sha256}/{tenant_id}")
    }

    /// Prefix used to list ALL tenant markers for a given sha256.
    fn manifest_prefix(sha256: &str) -> String {
        format!("_manifest/{sha256}/")
    }

    /// Returns `true` if ANY tenant other than `requester_tenant` has a
    /// manifest marker for `sha256`.
    async fn exists_under_other_tenant(
        &self,
        sha256: &str,
        requester_tenant: &uuid::Uuid,
    ) -> Result<bool, BlobError> {
        let prefix = Self::manifest_prefix(sha256);
        let requester_str = requester_tenant.to_string();

        let resp = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .send()
            .await
            .map_err(|e| BlobError::S3(classify_sdk_err(&e)))?;

        let contents = resp.contents();
        for obj in contents {
            if let Some(key) = obj.key() {
                // Extract the last path segment (the tenant_id).
                let tenant_segment = key.trim_start_matches(&prefix);
                if tenant_segment != requester_str {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[async_trait]
impl BlobStore for S3Store {
    #[instrument(skip(self, data), fields(tenant=%blob_ref.tenant_id, sha256=%blob_ref.sha256))]
    async fn put(&self, blob_ref: &BlobRef, data: Bytes) -> Result<(), BlobError> {
        let actual_size = data.len() as u64;
        if actual_size != blob_ref.size {
            return Err(BlobError::SizeMismatch {
                expected: blob_ref.size,
                got: actual_size,
            });
        }
        let actual_sha256 = compute_sha256(&data);
        if actual_sha256 != blob_ref.sha256 {
            return Err(BlobError::Sha256Mismatch {
                expected: blob_ref.sha256.clone(),
                got: actual_sha256,
            });
        }

        // INVARIANT (B-HIGH-2): Write the per-tenant manifest marker BEFORE
        // writing the blob.  If the manifest write fails we abort, leaving no
        // visible blob.  An orphaned manifest (manifest present, blob absent)
        // is observable only as a NotFound — not a security breach.  An
        // orphaned blob (blob present, manifest absent) would be a breach, so
        // we must never reach that state.
        let manifest_key = Self::manifest_key(&blob_ref.sha256, &blob_ref.tenant_id);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&manifest_key)
            .body(aws_sdk_s3::primitives::ByteStream::from(Bytes::new()))
            .send()
            .await
            .map_err(|e| BlobError::S3(classify_sdk_err(&e)))?;

        // Write blob object only after manifest marker is durable.
        let key = Self::object_key(blob_ref);
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .content_type(&blob_ref.content_type)
            .body(aws_sdk_s3::primitives::ByteStream::from(data))
            .send()
            .await
            .map_err(|e| BlobError::S3(classify_sdk_err(&e)))?;

        Ok(())
    }

    #[instrument(skip(self), fields(tenant=%blob_ref.tenant_id, sha256=%blob_ref.sha256))]
    async fn get(&self, blob_ref: &BlobRef) -> Result<Bytes, BlobError> {
        let key = Self::object_key(blob_ref);
        match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(resp) => {
                let data = resp
                    .body
                    .collect()
                    .await
                    .map_err(|e| BlobError::S3(S3ErrorKind::Other(e.to_string())))?
                    .into_bytes();
                Ok(data)
            }
            Err(SdkError::ServiceError(ref se))
                if matches!(se.err(), GetObjectError::NoSuchKey(_)) =>
            {
                // Blob not under this tenant — check whether another tenant
                // has a manifest marker for this sha256 (B-HIGH-1).
                if self
                    .exists_under_other_tenant(&blob_ref.sha256, &blob_ref.tenant_id)
                    .await?
                {
                    Err(BlobError::TenantMismatch)
                } else {
                    Err(BlobError::NotFound {
                        tenant_id: blob_ref.tenant_id,
                        sha256: blob_ref.sha256.clone(),
                    })
                }
            }
            Err(e) => Err(BlobError::S3(classify_sdk_err(&e))),
        }
    }

    #[instrument(skip(self), fields(tenant=%blob_ref.tenant_id, sha256=%blob_ref.sha256))]
    async fn delete(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        // Verify ownership before deleting.
        let head = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(Self::object_key(blob_ref))
            .send()
            .await;

        match head {
            Ok(_) => {
                // Delete the blob object.
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(Self::object_key(blob_ref))
                    .send()
                    .await
                    .map_err(|e| BlobError::S3(classify_sdk_err(&e)))?;

                // Remove the per-tenant manifest marker so it doesn't outlive
                // the blob for this tenant (B-HIGH-1).
                let manifest_key = Self::manifest_key(&blob_ref.sha256, &blob_ref.tenant_id);
                self.client
                    .delete_object()
                    .bucket(&self.bucket)
                    .key(&manifest_key)
                    .send()
                    .await
                    .map_err(|e| BlobError::S3(classify_sdk_err(&e)))?;

                Ok(())
            }
            Err(SdkError::ServiceError(ref se))
                if matches!(se.err(), HeadObjectError::NotFound(_)) =>
            {
                // Check if another tenant owns this sha256.
                if self
                    .exists_under_other_tenant(&blob_ref.sha256, &blob_ref.tenant_id)
                    .await?
                {
                    Err(BlobError::TenantMismatch)
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(BlobError::S3(classify_sdk_err(&e))),
        }
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        let result = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(Self::object_key(blob_ref))
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(SdkError::ServiceError(ref se))
                if matches!(se.err(), HeadObjectError::NotFound(_)) =>
            {
                Ok(false)
            }
            Err(e) => Err(BlobError::S3(classify_sdk_err(&e))),
        }
    }
}

#[cfg(all(test, feature = "s3"))]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// Returns the store only when `TEST_S3_ENDPOINT` (or `RB_BLOB_S3_ENDPOINT`) is set;
    /// otherwise the test is skipped via `None`.
    async fn try_store() -> Option<S3Store> {
        // Accept either env var so the caller can set whichever is convenient.
        if let Ok(endpoint) = std::env::var("TEST_S3_ENDPOINT") {
            // SAFETY: single-threaded test setup; no other threads are reading env at this point.
            unsafe { std::env::set_var("RB_BLOB_S3_ENDPOINT", endpoint) };
        }
        if std::env::var("RB_BLOB_S3_ENDPOINT").is_err() {
            eprintln!("Skipping S3 test — set TEST_S3_ENDPOINT or RB_BLOB_S3_ENDPOINT to enable");
            return None;
        }
        Some(S3Store::from_env().await.expect("S3Store::from_env"))
    }

    fn make_blob_ref(tenant_id: Uuid, data: &[u8]) -> BlobRef {
        let sha256 = compute_sha256(data);
        BlobRef::new(
            tenant_id,
            sha256,
            "application/octet-stream",
            data.len() as u64,
        )
    }

    /// Basic round-trip: put → exists → get → delete → !exists
    ///
    /// Requires localstack running at `RB_BLOB_S3_ENDPOINT` with a pre-created bucket.
    /// Run with: `TEST_S3_ENDPOINT=http://localhost:4566 cargo test -p rb-blob s3_roundtrip --features s3`
    #[tokio::test]
    async fn s3_roundtrip() {
        let Some(store) = try_store().await else {
            return;
        };
        let tenant = Uuid::new_v4();
        let data = b"hello s3 blob store";
        let blob_ref = make_blob_ref(tenant, data);

        store
            .put(&blob_ref, Bytes::from_static(data))
            .await
            .expect("put");
        assert!(store.exists(&blob_ref).await.expect("exists"));

        let retrieved = store.get(&blob_ref).await.expect("get");
        assert_eq!(retrieved.as_ref(), data);

        store.delete(&blob_ref).await.expect("delete");
        assert!(!store.exists(&blob_ref).await.expect("exists after delete"));
    }

    /// A puts → B tries get → `TenantMismatch`
    #[tokio::test]
    async fn s3_tenant_isolation() {
        let Some(store) = try_store().await else {
            return;
        };
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let data = b"shared content different owners";
        let ref_a = make_blob_ref(tenant_a, data);

        store
            .put(&ref_a, Bytes::from_static(data))
            .await
            .expect("put as tenant_a");

        let ref_b = BlobRef::new(
            tenant_b,
            ref_a.sha256.clone(),
            "application/octet-stream",
            ref_a.size,
        );
        let err = store.get(&ref_b).await.expect_err("cross-tenant must fail");
        assert!(
            matches!(err, BlobError::TenantMismatch),
            "expected TenantMismatch, got {err:?}"
        );

        // Clean up A's blob.
        store.delete(&ref_a).await.expect("cleanup");
    }

    /// A puts → B puts → A deletes → C gets must return `TenantMismatch` (B still owns).
    /// After B also deletes, C gets must return `NotFound`.
    /// This exercises the per-tenant manifest isolation under the production path
    /// (not a test-double that auto-preserves headers / markers).
    #[tokio::test]
    async fn s3_multi_tenant_delete_isolation() {
        let Some(store) = try_store().await else {
            return;
        };
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let tenant_c = Uuid::new_v4();
        let data = b"shared content multi-tenant s3 delete";
        let ref_a = make_blob_ref(tenant_a, data);
        let ref_b = BlobRef::new(
            tenant_b,
            ref_a.sha256.clone(),
            "application/octet-stream",
            ref_a.size,
        );
        let ref_c = BlobRef::new(
            tenant_c,
            ref_a.sha256.clone(),
            "application/octet-stream",
            ref_a.size,
        );

        store
            .put(&ref_a, Bytes::from_static(data))
            .await
            .expect("A put");
        store
            .put(&ref_b, Bytes::from_static(data))
            .await
            .expect("B put");
        store.delete(&ref_a).await.expect("A delete");

        // B still owns → C must see TenantMismatch, not NotFound.
        let err = store
            .get(&ref_c)
            .await
            .expect_err("C get while B owns blob");
        assert!(
            matches!(err, BlobError::TenantMismatch),
            "expected TenantMismatch (B still owns blob), got {err:?}"
        );

        store.delete(&ref_b).await.expect("B delete");

        // Nobody owns → C must see NotFound.
        let err2 = store
            .get(&ref_c)
            .await
            .expect_err("C get after all-deleted");
        assert!(
            matches!(err2, BlobError::NotFound { .. }),
            "expected NotFound after all tenants deleted, got {err2:?}"
        );
    }

    /// A puts → B puts same sha256 → A deletes → B can still get (no `TenantMismatch`)
    /// This is the B-HIGH-1 regression test for per-tenant markers.
    #[tokio::test]
    async fn s3_per_tenant_manifest_independent() {
        let Some(store) = try_store().await else {
            return;
        };
        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let data = b"independently owned blob";
        let ref_a = make_blob_ref(tenant_a, data);
        let ref_b = BlobRef::new(
            tenant_b,
            ref_a.sha256.clone(),
            "application/octet-stream",
            ref_a.size,
        );

        // Both tenants store the same content.
        store
            .put(&ref_a, Bytes::from_static(data))
            .await
            .expect("put A");
        store
            .put(&ref_b, Bytes::from_static(data))
            .await
            .expect("put B");

        // A deletes their copy.
        store.delete(&ref_a).await.expect("delete A");

        // B can still retrieve their copy — no TenantMismatch, no NotFound.
        let retrieved = store.get(&ref_b).await.expect("B get after A delete");
        assert_eq!(retrieved.as_ref(), data);

        // Clean up.
        store.delete(&ref_b).await.expect("cleanup B");
    }
}
