//! `POST /v1/repos/{repo_id}/graph/query` — raw Cypher query against the
//! tenant's Neo4j graph (REQ-DP-07 stub; full implementation is out of scope
//! for this PR — see the health/consistency work in RUSAA-78).
//!
//! When `read_only = true` the request is pre-flighted through
//! `rb_storage_neo4j::write_check::has_write_operators` to reject writes
//! before they reach Neo4j.

use axum::{Json, extract::State};
use rb_storage_neo4j::has_write_operators;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_read_auth},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/repos/{repo_id}/graph/query`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct GraphQueryRequest {
    /// Cypher query string.
    pub cypher: String,
    /// When `true` (default), reject queries that contain write operators.
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_read_only() -> bool {
    true
}

/// Placeholder response — full Neo4j integration is a follow-on task.
#[derive(Debug, Serialize, ToSchema)]
pub struct GraphQueryResponse {
    pub columns: Vec<String>,
    pub rows: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Execute a Cypher query against the tenant's code-intelligence graph.
///
/// This endpoint is stubbed for RUSAA-78 (health/consistency PR). The
/// write-check pre-flight is implemented; the Neo4j execution layer is a
/// follow-on task.
#[utoipa::path(
    post,
    path = "/v1/repos/{repo_id}/graph/query",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID"),
    ),
    request_body = GraphQueryRequest,
    responses(
        (status = 200, description = "Query results", body = GraphQueryResponse),
        (status = 400, description = "Write operators detected in read-only query (cypher_write_denied)"),
        (status = 401, description = "Not authenticated"),
        (status = 503, description = "Neo4j graph store is not configured on this instance"),
    ),
    tag = "query"
)]
pub async fn post_graph_query(
    State(_state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path(_repo_id): axum::extract::Path<Uuid>,
    Json(body): Json<GraphQueryRequest>,
) -> Result<Json<GraphQueryResponse>, AppError> {
    let _tenant_id = require_read_auth(auth)?;

    if body.read_only && has_write_operators(&body.cypher) {
        return Err(AppError::CypherWriteDenied);
    }

    // Neo4j execution is a follow-on task (RUSAA-78 scope: health endpoints).
    Err(AppError::GraphNotConfigured)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_defaults_to_true() {
        let req: GraphQueryRequest =
            serde_json::from_str(r#"{"cypher":"MATCH (n) RETURN n"}"#).unwrap();
        assert!(req.read_only);
    }

    #[test]
    fn read_only_can_be_disabled() {
        let req: GraphQueryRequest =
            serde_json::from_str(r#"{"cypher":"CREATE (n:Foo)","read_only":false}"#).unwrap();
        assert!(!req.read_only);
    }

    #[test]
    fn write_check_detects_create() {
        assert!(has_write_operators("CREATE (n:Foo)"));
    }

    #[test]
    fn write_check_allows_match() {
        assert!(!has_write_operators("MATCH (n) RETURN n"));
    }
}
