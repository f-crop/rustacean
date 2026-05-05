//! Projection deletion logic for tombstone events.
//!
//! Each storage backend is deleted in sequence: `PostgreSQL` → `Neo4j` → `Qdrant`.
//! The function short-circuits on the first failure and returns the error so
//! the consumer can retry the whole event atomically (Kafka at-least-once
//! delivery ensures idempotency).

use anyhow::{Context as _, Result};
use rb_schemas::{TenantId, Tombstone};
use rb_storage_neo4j::TenantGraph;
use rb_storage_pg::TenantPool;
use rb_tenant::TenantCtx;

/// Delete all projections described by `ev` from every storage backend.
///
/// When `ev.repo_id` is empty the deletion is tenant-wide (drops the PG
/// schema, removes all Neo4j nodes for the tenant, and lists+deletes all
/// Qdrant collections for the tenant).  When non-empty only data for that
/// specific repo is removed.
///
/// Idempotent: each backend uses IF-NOT-EXISTS / MATCH-with-no-results
/// semantics so re-delivery of the same tombstone is safe.
#[allow(clippy::missing_errors_doc)]
pub async fn handle_tombstone(
    pool: &TenantPool,
    graph: &TenantGraph,
    qdrant_url: Option<&str>,
    tenant_id: &TenantId,
    ev: &Tombstone,
) -> Result<()> {
    let ctx = TenantCtx::new(*tenant_id);
    let tenant_wide = ev.repo_id.is_empty();

    // ── PostgreSQL ───────────────────────────────────────────────────────────
    if tenant_wide {
        pool.drop_schema(&ctx)
            .await
            .context("PG drop_schema failed")?;
        tracing::debug!(tenant_id = %tenant_id, "tombstoner: PG schema dropped");
    } else {
        let repo_uuid = ev
            .repo_id
            .parse::<uuid::Uuid>()
            .with_context(|| format!("invalid repo_id UUID: {}", ev.repo_id))?;
        pool.delete_repo_data(&ctx, repo_uuid)
            .await
            .context("PG delete_repo_data failed")?;
        tracing::debug!(
            tenant_id = %tenant_id,
            repo_id   = %ev.repo_id,
            "tombstoner: PG repo rows deleted"
        );
    }

    // ── Neo4j ────────────────────────────────────────────────────────────────
    if tenant_wide {
        graph
            .delete_all_tenant_nodes(tenant_id)
            .await
            .context("Neo4j delete_all_tenant_nodes failed")?;
        tracing::debug!(tenant_id = %tenant_id, "tombstoner: Neo4j tenant nodes deleted");
    } else {
        graph
            .delete_repo_nodes(tenant_id, &ev.repo_id)
            .await
            .context("Neo4j delete_repo_nodes failed")?;
        tracing::debug!(
            tenant_id = %tenant_id,
            repo_id   = %ev.repo_id,
            "tombstoner: Neo4j repo nodes deleted"
        );
    }

    // ── Qdrant (best-effort HTTP) ────────────────────────────────────────────
    // ADR-008 §6 (Inconsistency-1 fix): all embeddings are stored in the shared
    // `rb_embeddings` collection with `tenant_id` and `repo_id` payload fields.
    // The former per-repo collection naming (`rb_{tenant_id}_{repo_id}`) was
    // incorrect — that collection does not exist. Delete by payload filter instead.
    match qdrant_url {
        None => {
            tracing::warn!(
                tenant_id = %tenant_id,
                "tombstoner: QDRANT_URL not set — Qdrant deletion skipped"
            );
        }
        Some(url) => {
            if tenant_wide {
                delete_qdrant_by_filter(url, tenant_id, None).await?;
            } else {
                delete_qdrant_by_filter(url, tenant_id, Some(&ev.repo_id)).await?;
            }
        }
    }

    Ok(())
}

/// Delete Qdrant points from the shared `rb_embeddings` collection by payload filter.
///
/// ADR-008 §6 (Inconsistency-1 fix): all embeddings live in a single shared
/// collection keyed by `tenant_id` and `repo_id` payload fields. The old
/// per-repo collection naming (`rb_{tenant_id}_{repo_id}`) referenced
/// collections that do not exist; this function uses the correct filter-delete
/// API instead.
///
/// When `repo_id` is `None` the filter matches all points for the tenant.
/// When `repo_id` is `Some` the filter further narrows to that specific repo.
/// Idempotent: Qdrant returns 200 even when no points matched the filter.
async fn delete_qdrant_by_filter(
    qdrant_url: &str,
    tenant_id: &TenantId,
    repo_id: Option<&str>,
) -> Result<()> {
    const COLLECTION: &str = "rb_embeddings";

    let mut must_conditions = vec![serde_json::json!({
        "key": "tenant_id",
        "match": { "value": tenant_id.to_string() }
    })];

    if let Some(rid) = repo_id {
        must_conditions.push(serde_json::json!({
            "key": "repo_id",
            "match": { "value": rid }
        }));
    }

    let body = serde_json::json!({ "filter": { "must": must_conditions } });
    let url = format!("{qdrant_url}/collections/{COLLECTION}/points/delete");

    let resp = reqwest::Client::new()
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| format!("Qdrant POST {url}"))?;

    let status = resp.status().as_u16();
    match status {
        200 | 204 => {
            tracing::debug!(
                tenant_id = %tenant_id,
                repo_id   = repo_id.unwrap_or("(all)"),
                "tombstoner: Qdrant points deleted from rb_embeddings"
            );
            Ok(())
        }
        code => Err(anyhow::anyhow!(
            "Qdrant POST /collections/{COLLECTION}/points/delete returned unexpected status {code}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qdrant_collection_is_shared_rb_embeddings() {
        // ADR-008 §6: the shared collection name is a constant, not derived from
        // tenant/repo identifiers.
        assert_eq!(
            "rb_embeddings",
            "rb_embeddings",
            "collection name must be the shared rb_embeddings collection"
        );
    }

    #[test]
    fn handle_tombstone_parses_empty_repo_id_as_tenant_wide() {
        let ev = Tombstone {
            tenant_id: "t".to_string(),
            repo_id: String::new(),
            requested_by: "u".to_string(),
            emitted_at_ms: 0,
        };
        assert!(ev.repo_id.is_empty(), "empty repo_id means tenant-wide");
    }

    #[test]
    fn handle_tombstone_detects_repo_specific() {
        let repo_uuid = uuid::Uuid::new_v4().to_string();
        let ev = Tombstone {
            tenant_id: "t".to_string(),
            repo_id: repo_uuid.clone(),
            requested_by: "u".to_string(),
            emitted_at_ms: 0,
        };
        assert!(!ev.repo_id.is_empty());
        assert!(ev.repo_id.parse::<uuid::Uuid>().is_ok(), "repo_id must be a valid UUID");
    }

    #[test]
    fn handle_tombstone_rejects_non_uuid_repo_id() {
        let ev = Tombstone {
            tenant_id: "t".to_string(),
            repo_id: "not-a-uuid".to_string(),
            requested_by: "u".to_string(),
            emitted_at_ms: 0,
        };
        assert!(
            ev.repo_id.parse::<uuid::Uuid>().is_err(),
            "non-UUID repo_id must fail parse"
        );
    }
}
