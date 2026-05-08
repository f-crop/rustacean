//! Qdrant ANN search helper for `rb-storage-qdrant`.
//!
//! Performs cosine-similarity queries against the `rb_embeddings` collection
//! with a mandatory `tenant_id` `must` filter to prevent cross-tenant data
//! leakage (ADR-008 §2, §3.1 security constraint).
//!
//! One extra result beyond `limit` is requested to detect whether a next page
//! exists without a separate count query.

use serde_json::json;

use crate::{COLLECTION, QdrantError};

/// Parameters for a single ANN search query.
pub struct SearchOptions<'a> {
    /// Base URL of the Qdrant instance (e.g. `http://qdrant:6333`).
    pub qdrant_url: &'a str,
    /// Tenant UUID string — MUST be included in every Qdrant query.
    pub tenant_id: &'a str,
    /// Pre-computed embedding vector for the query string.
    pub query_vector: &'a [f32],
    /// Optional repository UUID filter applied as an additional `must` clause.
    pub repo_id_filter: Option<&'a str>,
    /// Minimum cosine similarity score. Hits below this threshold are dropped.
    pub score_threshold: f32,
    /// Maximum number of hits to return (before pagination detection).
    pub limit: u32,
    /// Number of leading hits to skip (cursor-based pagination offset).
    pub offset: u32,
}

/// A single hit returned by a Qdrant ANN search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Fully-qualified name of the code symbol.
    pub fqn: String,
    /// Repository UUID string from the point payload.
    pub repo_id: String,
    /// Cosine similarity score — higher is more relevant.
    pub score: f32,
    /// Ingestion run UUID string that produced this embedding.
    /// Used as `last_ingest_trace_id` in API responses (UUID hex sans dashes).
    pub ingest_run_id: String,
}

/// Results of a single Qdrant search query.
#[derive(Debug)]
pub struct SearchResults {
    /// Hits ordered by descending score, truncated to `limit`.
    pub hits: Vec<SearchHit>,
    /// Pagination offset for the next page; `None` when there are no further results.
    pub next_offset: Option<u32>,
}

/// Perform an ANN search against `rb_embeddings`.
///
/// The `tenant_id` filter is always applied as a Qdrant `must` condition —
/// this is the structural guarantee that prevents cross-tenant data leakage.
/// Additional optional filters (`repo_id`) are composed on top.
///
/// # Errors
///
/// Returns [`QdrantError`] if the HTTP request fails or Qdrant returns a
/// non-success status code.
pub async fn search(
    http: &reqwest::Client,
    opts: SearchOptions<'_>,
) -> Result<SearchResults, QdrantError> {
    let mut must_conditions =
        vec![json!({ "key": "tenant_id", "match": { "value": opts.tenant_id } })];

    if let Some(repo_id) = opts.repo_id_filter {
        must_conditions.push(json!({ "key": "repo_id", "match": { "value": repo_id } }));
    }

    // Request one extra hit to detect the presence of a next page without
    // a separate count query.
    let request_limit = opts.limit + 1;

    let body = json!({
        "vector": opts.query_vector,
        "limit": request_limit,
        "offset": opts.offset,
        "score_threshold": opts.score_threshold,
        "with_payload": ["fqn", "repo_id", "ingest_run_id"],
        "filter": { "must": must_conditions },
    });

    let url = format!(
        "{}/collections/{}/points/search",
        opts.qdrant_url, COLLECTION
    );

    let resp = http.post(&url).json(&body).send().await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(QdrantError::Http {
            status: status.as_u16(),
            body,
        });
    }

    let json_val: serde_json::Value = resp.json().await?;

    let points = json_val
        .get("result")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut all_hits: Vec<SearchHit> = points
        .iter()
        .filter_map(|pt| {
            let payload = pt.get("payload")?;
            let fqn = payload.get("fqn")?.as_str()?.to_owned();
            let repo_id = payload.get("repo_id")?.as_str()?.to_owned();
            let ingest_run_id = payload
                .get("ingest_run_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_owned();
            #[allow(clippy::cast_possible_truncation)]
            let score = pt.get("score")?.as_f64()? as f32;
            Some(SearchHit {
                fqn,
                repo_id,
                score,
                ingest_run_id,
            })
        })
        .collect();

    let has_more = all_hits.len() > opts.limit as usize;
    if has_more {
        all_hits.truncate(opts.limit as usize);
    }

    let next_offset = has_more.then_some(opts.offset + opts.limit);

    Ok(SearchResults {
        hits: all_hits,
        next_offset,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_hit_fields_accessible() {
        let hit = SearchHit {
            fqn: "crate::mod::Fn".to_owned(),
            repo_id: "00000000-0000-0000-0000-000000000001".to_owned(),
            score: 0.75,
            ingest_run_id: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_owned(),
        };
        assert!(hit.score > 0.0);
        assert!(!hit.fqn.is_empty());
        assert!(!hit.repo_id.is_empty());
        assert!(!hit.ingest_run_id.is_empty());
    }

    #[test]
    fn search_results_next_offset_when_has_more() {
        let result = SearchResults {
            hits: vec![SearchHit {
                fqn: "a::Foo".into(),
                repo_id: "r1".into(),
                score: 0.9,
                ingest_run_id: "run1".into(),
            }],
            next_offset: Some(10),
        };
        assert_eq!(result.next_offset, Some(10));
    }

    #[test]
    fn search_results_no_next_offset_when_exhausted() {
        let result = SearchResults {
            hits: vec![],
            next_offset: None,
        };
        assert!(result.next_offset.is_none());
    }
}
