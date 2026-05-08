//! Tenant-scoped Qdrant vector search.
//!
//! Every search injects a `must` filter on `tenant_id` (ADR-007 §13.2) so
//! cross-tenant data is never reachable even on bugs in call sites.
//!
//! No caller outside `rb-storage-qdrant` may issue raw Qdrant requests —
//! use this type as the sole search path.

use rb_schemas::TenantId;
use serde_json::json;
use uuid::Uuid;

use crate::error::QdrantError;

/// Qdrant collection used for all code embeddings.
const COLLECTION: &str = "rb_embeddings";

/// A single ranked result from a Qdrant nearest-neighbour search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Fully-qualified name of the matched code symbol.
    pub fqn: String,
    /// Repository UUID string from the point payload.
    pub repo_id: String,
    /// Cosine similarity score in `[0, 1]` (higher is more similar).
    pub score: f32,
}

/// Tenant-scoped Qdrant client.
///
/// All search queries must pass through [`Self::search`]; the method injects a
/// mandatory `tenant_id` `must` filter before forwarding to Qdrant so that
/// per-tenant isolation (ADR-007 §13.2) is enforced at the driver level.
pub struct TenantVectorStore {
    client: reqwest::Client,
    url: String,
}

impl TenantVectorStore {
    /// Build a store pointing at `qdrant_url` (e.g. `http://qdrant:6333`).
    #[must_use]
    pub fn new(qdrant_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            url: qdrant_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Search `rb_embeddings` for the `k` nearest neighbours of `vector`.
    ///
    /// Always injects a `must` filter on `tenant_id` — refusing to execute
    /// when the tenant is somehow invalid is safer than silently returning
    /// cross-tenant results.  An additional optional `repo_id` filter narrows
    /// results to a single repository.
    ///
    /// # Errors
    ///
    /// - [`QdrantError::MissingTenantFilter`] — internal guard (should never
    ///   fire; present to make the invariant explicit in the type system).
    /// - [`QdrantError::Http`] — Qdrant returned a non-2xx status.
    /// - [`QdrantError::Request`] — network-level failure.
    /// - [`QdrantError::Parse`] — unexpected response shape.
    #[allow(clippy::cast_possible_truncation)]
    pub async fn search(
        &self,
        tenant_id: &TenantId,
        vector: &[f32],
        limit: u32,
        repo_id: Option<Uuid>,
    ) -> Result<Vec<SearchHit>, QdrantError> {
        let tenant_str = tenant_id.to_string();
        if tenant_str.is_empty() {
            return Err(QdrantError::MissingTenantFilter);
        }

        // Build must-filter conditions.
        let mut must_conditions = vec![json!({
            "key": "tenant_id",
            "match": { "value": tenant_str }
        })];

        if let Some(rid) = repo_id {
            must_conditions.push(json!({
                "key": "repo_id",
                "match": { "value": rid.to_string() }
            }));
        }

        let body = json!({
            "vector": vector,
            "limit": limit,
            "with_payload": true,
            "filter": {
                "must": must_conditions
            }
        });

        let url = format!("{}/collections/{}/points/search", self.url, COLLECTION);
        let resp = self.client.post(&url).json(&body).send().await?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(QdrantError::Http {
                status: status.as_u16(),
                body: body_text,
            });
        }

        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| QdrantError::Parse(e.to_string()))?;

        let results = json
            .get("result")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| QdrantError::Parse("missing 'result' array".into()))?;

        let mut hits = Vec::with_capacity(results.len());
        for item in results {
            let score = item
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .map(|f| f as f32)
                .ok_or_else(|| QdrantError::Parse("missing 'score' field".into()))?;

            let payload = item
                .get("payload")
                .ok_or_else(|| QdrantError::Parse("missing 'payload' field".into()))?;

            let fqn = payload
                .get("fqn")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| QdrantError::Parse("missing payload.fqn".into()))?
                .to_owned();

            let repo_id_str = payload
                .get("repo_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| QdrantError::Parse("missing payload.repo_id".into()))?
                .to_owned();

            hits.push(SearchHit {
                fqn,
                repo_id: repo_id_str,
                score,
            });
        }

        Ok(hits)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_trims_trailing_slash_from_url() {
        let store = TenantVectorStore::new("http://qdrant:6333/");
        assert_eq!(store.url, "http://qdrant:6333");
    }

    #[test]
    fn search_hit_fields_accessible() {
        let hit = SearchHit {
            fqn: "my_crate::Foo".to_owned(),
            repo_id: "repo-uuid".to_owned(),
            score: 0.95,
        };
        assert_eq!(hit.fqn, "my_crate::Foo");
        assert!((hit.score - 0.95).abs() < f32::EPSILON);
    }

    #[test]
    fn collection_constant_matches_embed_worker() {
        assert_eq!(COLLECTION, "rb_embeddings");
    }
}
