//! Qdrant REST client helpers for `embed-worker`.
//!
//! Uses a single `rb_embeddings` collection with payload multi-tenancy per
//! ADR-007 §13.2.  Every point carries `tenant_id` in its payload so
//! per-tenant filtering is possible without per-tenant collections.
//!
//! Point ID = first 16 bytes of SHA-256( "{tenant}:{repo}:{fqn}" ) interpreted
//! as a UUID.  This gives stable, idempotent IDs (upsert semantics).

use anyhow::{Context as _, Result};
use rb_schemas::TenantId;
use serde_json::json;
use sha2::{Digest as _, Sha256};

/// Qdrant collection used for all embeddings.
pub(crate) const COLLECTION: &str = "rb_embeddings";

/// Ensure the `rb_embeddings` collection exists with the expected vector size.
///
/// If the collection does not exist it is created.  If it exists but has a
/// different vector size the function returns an error so the binary exits
/// fast (startup guard per ADR-007 §11.8).
pub(crate) async fn ensure_collection(qdrant_url: &str, dimensions: u32) -> Result<()> {
    let http = reqwest::Client::new();
    let info_url = format!("{qdrant_url}/collections/{COLLECTION}");

    let resp = http
        .get(&info_url)
        .send()
        .await
        .context("failed to reach Qdrant")?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        create_collection(&http, qdrant_url, dimensions).await?;
        tracing::info!(
            dimensions,
            "embed_worker: created Qdrant collection {COLLECTION}"
        );
        return Ok(());
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Qdrant GET /collections/{COLLECTION} returned {status}: {body}");
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("Qdrant info response is not JSON")?;
    let actual_dims = body
        .pointer("/result/config/params/vectors/size")
        .and_then(serde_json::Value::as_u64)
        .context("could not read vectors.size from Qdrant collection info")?;

    if actual_dims != u64::from(dimensions) {
        anyhow::bail!(
            "Qdrant collection '{COLLECTION}' has vector size {actual_dims} but \
             RB_EMBEDDING_DIMENSIONS={dimensions}; fix the mismatch before starting"
        );
    }

    tracing::info!(
        dimensions,
        "embed_worker: Qdrant collection {COLLECTION} validated (size={actual_dims})"
    );
    Ok(())
}

/// Upsert one embedding vector into `rb_embeddings`.
///
/// `point_id` is a UUID derived from SHA-256 of `"{tenant}:{repo}:{fqn}"`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_vector(
    http: &reqwest::Client,
    qdrant_url: &str,
    tenant_id: TenantId,
    repo_id: &str,
    fqn: &str,
    ingest_run_id: &str,
    embedding_model: &str,
    dimensions: u32,
    vector: Vec<f32>,
) -> Result<()> {
    if vector.len() != dimensions as usize {
        anyhow::bail!(
            "vector length {} does not match expected dimensions {}",
            vector.len(),
            dimensions
        );
    }

    let point_id = point_id_for(tenant_id, repo_id, fqn);

    let url = format!("{qdrant_url}/collections/{COLLECTION}/points?wait=true");
    let body = json!({
        "points": [{
            "id": point_id,
            "vector": vector,
            "payload": {
                "tenant_id": tenant_id.to_string(),
                "repo_id": repo_id,
                "fqn": fqn,
                "ingest_run_id": ingest_run_id,
                "embedding_model": embedding_model,
            }
        }]
    });

    let resp = http
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("Qdrant upsert request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Qdrant upsert returned HTTP {status}: {text}");
    }

    Ok(())
}

/// Create the `rb_embeddings` collection with a flat vector config.
async fn create_collection(
    http: &reqwest::Client,
    qdrant_url: &str,
    dimensions: u32,
) -> Result<()> {
    let url = format!("{qdrant_url}/collections/{COLLECTION}");
    let body = json!({
        "vectors": {
            "size": dimensions,
            "distance": "Cosine"
        }
    });

    let resp = http
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("Qdrant create collection request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Qdrant create collection returned HTTP {status}: {text}");
    }

    // Create the tenant_id payload index for efficient per-tenant filtering.
    create_tenant_index(http, qdrant_url).await?;

    Ok(())
}

/// Create a payload index on `tenant_id` for per-tenant filtering performance.
async fn create_tenant_index(http: &reqwest::Client, qdrant_url: &str) -> Result<()> {
    let url = format!("{qdrant_url}/collections/{COLLECTION}/index");
    let body = json!({
        "field_name": "tenant_id",
        "field_schema": "keyword"
    });

    let resp = http
        .put(&url)
        .json(&body)
        .send()
        .await
        .context("Qdrant create index request failed")?;

    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("Qdrant create index returned HTTP {status}: {text}");
    }

    Ok(())
}

/// Derive a stable UUID point ID from `"{tenant}:{repo}:{fqn}"`.
///
/// Takes the first 16 bytes of SHA-256 and constructs a version-4-style UUID.
/// This gives per-item idempotency: re-embedding the same item overwrites the
/// existing point rather than creating a duplicate.
fn point_id_for(tenant_id: TenantId, repo_id: &str, fqn: &str) -> String {
    let raw = format!("{tenant_id}:{repo_id}:{fqn}");
    let hash = Sha256::digest(raw.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&hash[..16]);
    // Stamp as UUID v4 (random) layout — the content is a hash but the version
    // bits need not be semantically correct; what matters is uniqueness.
    let uuid = uuid::Uuid::from_bytes(bytes);
    uuid.to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_id_is_stable() {
        let tid: TenantId = "00000000-0000-0000-0000-000000000001"
            .parse::<uuid::Uuid>()
            .unwrap()
            .into();
        let id1 = point_id_for(tid, "repo-a", "src_lib::Foo");
        let id2 = point_id_for(tid, "repo-a", "src_lib::Foo");
        assert_eq!(id1, id2, "same inputs must produce the same point ID");
    }

    #[test]
    fn point_id_differs_for_different_fqn() {
        let tid: TenantId = "00000000-0000-0000-0000-000000000001"
            .parse::<uuid::Uuid>()
            .unwrap()
            .into();
        let id1 = point_id_for(tid, "repo-a", "src_lib::Foo");
        let id2 = point_id_for(tid, "repo-a", "src_lib::Bar");
        assert_ne!(id1, id2);
    }

    #[test]
    fn point_id_differs_for_different_tenant() {
        let t1: TenantId = "00000000-0000-0000-0000-000000000001"
            .parse::<uuid::Uuid>()
            .unwrap()
            .into();
        let t2: TenantId = "00000000-0000-0000-0000-000000000002"
            .parse::<uuid::Uuid>()
            .unwrap()
            .into();
        let id1 = point_id_for(t1, "repo-a", "src_lib::Foo");
        let id2 = point_id_for(t2, "repo-a", "src_lib::Foo");
        assert_ne!(id1, id2);
    }

    #[test]
    fn point_id_is_valid_uuid_string() {
        let tid: TenantId = "00000000-0000-0000-0000-000000000001"
            .parse::<uuid::Uuid>()
            .unwrap()
            .into();
        let id = point_id_for(tid, "repo", "mod::fn");
        assert!(
            uuid::Uuid::parse_str(&id).is_ok(),
            "point id must be a valid UUID"
        );
    }

    #[test]
    fn collection_constant() {
        assert_eq!(COLLECTION, "rb_embeddings");
    }
}
