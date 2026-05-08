//! Filesystem-backed blob store.
//!
//! ## Directory layout
//!
//! ```text
//! <base_path>/
//!   <tenant_id>/<sha256>          ← actual blob content
//!   _index/<sha256>/<tenant_id>   ← empty ownership marker (per-tenant, B-MED-1)
//! ```
//!
//! ## Atomic write invariant (B-MED-2)
//!
//! Blob data is written to `<path>.tmp`, fsynced, then renamed to the final
//! path.  A crash mid-write leaves only a `.tmp` file, which is invisible to
//! callers and can be cleaned up by an operator.
//!
//! ## Cross-tenant detection (B-MED-1)
//!
//! Instead of scanning all tenant directories on every GET miss (O(N tenants),
//! racy), we maintain a per-sha256 index directory.  On `put` we create an
//! empty file at `_index/<sha256>/<tenant_id>`.  On `delete` we remove it.
//! On a GET miss we list `_index/<sha256>/` and return `TenantMismatch` if any
//! entry belongs to a different tenant.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use tracing::instrument;

use crate::{BlobError, BlobRef, store::BlobStore};

/// Filesystem-backed blob store.
///
/// Layout: `<base_path>/<tenant_id>/<sha256>`
///
/// Tenant isolation is enforced by a per-sha256 index directory.
pub struct FilesystemStore {
    base_path: PathBuf,
    /// Mutex that guards concurrent writes to the `_index` directory to
    /// prevent TOCTOU races on the cross-tenant check.
    index_lock: Arc<Mutex<()>>,
}

impl FilesystemStore {
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
            index_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Reads `RB_BLOB_BASE_PATH` from the environment (default: `/var/lib/rustbrain/blobs`).
    ///
    /// # Errors
    ///
    /// Returns [`BlobError::Io`] if the base directory cannot be created.
    pub fn from_env() -> Result<Self, BlobError> {
        let path = std::env::var("RB_BLOB_BASE_PATH")
            .unwrap_or_else(|_| "/var/lib/rustbrain/blobs".to_string());
        Ok(Self::new(path))
    }

    fn blob_path(&self, blob_ref: &BlobRef) -> PathBuf {
        self.base_path
            .join(blob_ref.tenant_id.to_string())
            .join(&blob_ref.sha256)
    }

    /// Path to the per-tenant index marker for a given sha256.
    fn index_marker_path(&self, sha256: &str, tenant_id: &uuid::Uuid) -> PathBuf {
        self.base_path
            .join("_index")
            .join(sha256)
            .join(tenant_id.to_string())
    }

    /// Directory that holds all tenant markers for a given sha256.
    fn index_dir_path(&self, sha256: &str) -> PathBuf {
        self.base_path.join("_index").join(sha256)
    }

    /// Returns `true` when `sha256` has an index marker for ANY tenant other
    /// than the one in `blob_ref`.
    ///
    /// Must be called while holding `index_lock` to prevent TOCTOU races.
    async fn sha256_exists_other_tenant_locked(&self, blob_ref: &BlobRef) -> bool {
        let index_dir = self.index_dir_path(&blob_ref.sha256);
        let Ok(mut dir) = tokio::fs::read_dir(&index_dir).await else {
            return false;
        };
        let owner_str = blob_ref.tenant_id.to_string();
        while let Ok(Some(entry)) = dir.next_entry().await {
            if entry.file_name().to_string_lossy() != owner_str {
                return true;
            }
        }
        false
    }
}

fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[async_trait]
impl BlobStore for FilesystemStore {
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

        let path = self.blob_path(blob_ref);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        // 1. Write index marker FIRST under the lock (B-HIGH-2 ordering invariant:
        //    marker-exists ⟹ blob write was attempted; blob-without-marker is
        //    quarantined and invisible to callers).
        {
            let _guard = self.index_lock.lock().await;
            let marker = self.index_marker_path(&blob_ref.sha256, &blob_ref.tenant_id);
            if let Some(marker_parent) = marker.parent() {
                tokio::fs::create_dir_all(marker_parent).await?;
            }
            tokio::fs::File::create(&marker).await?;
        }

        // 2. Atomic blob write (B-MED-2): write to .tmp, fsync, rename to final path.
        let tmp_path = path.with_extension("tmp");
        {
            use tokio::io::AsyncWriteExt;
            let mut file = tokio::fs::File::create(&tmp_path).await?;
            file.write_all(&data).await?;
            file.sync_all().await?;
        }

        // fsync the parent directory to persist the rename durably on Linux.
        #[cfg(target_os = "linux")]
        if let Some(parent) = path.parent() {
            if let Ok(dir) = std::fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }

        tokio::fs::rename(&tmp_path, &path).await?;

        Ok(())
    }

    #[instrument(skip(self), fields(tenant=%blob_ref.tenant_id, sha256=%blob_ref.sha256))]
    async fn get(&self, blob_ref: &BlobRef) -> Result<Bytes, BlobError> {
        let path = self.blob_path(blob_ref);
        match tokio::fs::read(&path).await {
            Ok(data) => Ok(Bytes::from(data)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _guard = self.index_lock.lock().await;
                if self.sha256_exists_other_tenant_locked(blob_ref).await {
                    Err(BlobError::TenantMismatch)
                } else {
                    Err(BlobError::NotFound {
                        tenant_id: blob_ref.tenant_id,
                        sha256: blob_ref.sha256.clone(),
                    })
                }
            }
            Err(e) => Err(BlobError::Io(e)),
        }
    }

    #[instrument(skip(self), fields(tenant=%blob_ref.tenant_id, sha256=%blob_ref.sha256))]
    async fn delete(&self, blob_ref: &BlobRef) -> Result<(), BlobError> {
        let path = self.blob_path(blob_ref);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {
                // Remove the per-tenant index marker under the lock (B-MED-1).
                let _guard = self.index_lock.lock().await;
                let marker = self.index_marker_path(&blob_ref.sha256, &blob_ref.tenant_id);
                // Best-effort removal; if the marker is already gone we don't care.
                let _ = tokio::fs::remove_file(&marker).await;
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _guard = self.index_lock.lock().await;
                if self.sha256_exists_other_tenant_locked(blob_ref).await {
                    Err(BlobError::TenantMismatch)
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(BlobError::Io(e)),
        }
    }

    async fn exists(&self, blob_ref: &BlobRef) -> Result<bool, BlobError> {
        Ok(tokio::fs::metadata(self.blob_path(blob_ref)).await.is_ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn make_blob_ref(tenant_id: Uuid, data: &[u8]) -> BlobRef {
        let sha256 = compute_sha256(data);
        BlobRef::new(
            tenant_id,
            sha256,
            "application/octet-stream",
            data.len() as u64,
        )
    }

    #[tokio::test]
    async fn fs_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());
        let tenant = Uuid::new_v4();
        let data = b"hello, rustbrain blob store";
        let blob_ref = make_blob_ref(tenant, data);

        store
            .put(&blob_ref, Bytes::from_static(data))
            .await
            .expect("put");
        assert!(store.exists(&blob_ref).await.expect("exists"));

        let retrieved = store.get(&blob_ref).await.expect("get");
        assert_eq!(retrieved.as_ref(), data);
    }

    #[tokio::test]
    async fn tenant_isolation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());

        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let data = b"shared content different owners";
        let ref_a = make_blob_ref(tenant_a, data);

        // Tenant A stores the blob.
        store
            .put(&ref_a, Bytes::from_static(data))
            .await
            .expect("put as tenant_a");

        // Tenant B tries to read the same SHA-256 — must be rejected.
        let ref_b = BlobRef::new(
            tenant_b,
            ref_a.sha256.clone(),
            "application/octet-stream",
            ref_a.size,
        );
        let err = store
            .get(&ref_b)
            .await
            .expect_err("cross-tenant read must fail");
        assert!(
            matches!(err, BlobError::TenantMismatch),
            "expected TenantMismatch, got {err:?}"
        );
    }

    /// A puts → B puts → A deletes → C gets must return `TenantMismatch` (not `NotFound`),
    /// because B still owns the blob. After B also deletes, C must get `NotFound`.
    #[tokio::test]
    async fn multi_tenant_delete_isolation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());

        let tenant_a = Uuid::new_v4();
        let tenant_b = Uuid::new_v4();
        let tenant_c = Uuid::new_v4();
        let data = b"shared content multi-tenant delete";
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

        // B still owns the blob → C must see TenantMismatch, not NotFound.
        let err = store
            .get(&ref_c)
            .await
            .expect_err("C get must fail while B owns blob");
        assert!(
            matches!(err, BlobError::TenantMismatch),
            "expected TenantMismatch (B still owns blob), got {err:?}"
        );

        store.delete(&ref_b).await.expect("B delete");

        // Nobody owns the blob now → C must see NotFound.
        let err2 = store
            .get(&ref_c)
            .await
            .expect_err("C get after all-deleted must fail");
        assert!(
            matches!(err2, BlobError::NotFound { .. }),
            "expected NotFound after all tenants deleted, got {err2:?}"
        );
    }

    /// Simulates a crash between tmp-write and rename (B-MED-2).
    /// The interrupted .tmp must not be visible; a subsequent put must succeed
    /// and return the correct (non-corrupted) content.
    #[tokio::test]
    async fn atomic_put_crash_injection() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());
        let tenant = Uuid::new_v4();
        let data = b"correct content";
        let blob_ref = make_blob_ref(tenant, data);

        // Pre-create a stale .tmp with wrong content to simulate an aborted write.
        let path = store.blob_path(&blob_ref);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        let tmp_path = path.with_extension("tmp");
        tokio::fs::write(&tmp_path, b"corrupted partial write")
            .await
            .unwrap();

        // The .tmp must not be visible as an existing blob.
        assert!(!store.exists(&blob_ref).await.expect("exists before put"));

        // A normal put must overwrite the stale .tmp and rename to final path.
        store
            .put(&blob_ref, Bytes::from_static(data))
            .await
            .expect("put after crash");

        let retrieved = store
            .get(&blob_ref)
            .await
            .expect("get after crash-recovery put");
        assert_eq!(
            retrieved.as_ref(),
            data,
            "must return correct (non-corrupted) content"
        );
    }

    /// A puts → B puts same sha256 → A deletes → B can still get (no `TenantMismatch`)
    #[tokio::test]
    async fn per_tenant_index_independent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());

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

        store
            .put(&ref_a, Bytes::from_static(data))
            .await
            .expect("put A");
        store
            .put(&ref_b, Bytes::from_static(data))
            .await
            .expect("put B");

        // A deletes — B should still be accessible.
        store.delete(&ref_a).await.expect("delete A");

        let retrieved = store.get(&ref_b).await.expect("B get after A delete");
        assert_eq!(retrieved.as_ref(), data);
    }

    #[tokio::test]
    async fn delete_removes_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());
        let tenant = Uuid::new_v4();
        let data = b"to be deleted";
        let blob_ref = make_blob_ref(tenant, data);

        store
            .put(&blob_ref, Bytes::from_static(data))
            .await
            .expect("put");
        store.delete(&blob_ref).await.expect("delete");
        assert!(!store.exists(&blob_ref).await.expect("exists after delete"));
    }

    #[tokio::test]
    async fn sha256_mismatch_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());
        let tenant = Uuid::new_v4();
        let data = b"real data";
        let mut blob_ref = make_blob_ref(tenant, data);
        blob_ref.sha256 = "0".repeat(64); // wrong hash

        let err = store
            .put(&blob_ref, Bytes::from_static(data))
            .await
            .expect_err("sha256 mismatch should fail");
        assert!(matches!(err, BlobError::Sha256Mismatch { .. }));
    }

    #[tokio::test]
    async fn large_blob() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = FilesystemStore::new(dir.path());
        let tenant = Uuid::new_v4();

        // 100 MiB
        let size: usize = 100 * 1024 * 1024;
        #[allow(clippy::cast_possible_truncation)]
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let blob_ref = make_blob_ref(tenant, &data);

        store
            .put(&blob_ref, Bytes::from(data.clone()))
            .await
            .expect("put 100 MiB");
        let retrieved = store.get(&blob_ref).await.expect("get 100 MiB");
        assert_eq!(retrieved.len(), size);
        assert_eq!(retrieved.as_ref(), data.as_slice());
    }
}
