//! `POST /v1/graph/query` — raw Cypher query with tenant label injection (REQ-DP-05).
//!
//! Admin scope only: API key with `admin` scope or session with owner/admin role.
//! When `read_only=true` (default), the query is pre-flight scanned for write
//! operators and rejected with 400 `cypher_write_denied` if any are found.

use axum::{Json, extract::State, response::IntoResponse};
use rb_schemas::TenantId;
use rb_storage_neo4j::has_write_operators;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope, require_verified_session},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Request / response schemas
// ---------------------------------------------------------------------------

/// Request body for `POST /v1/graph/query`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct GraphQueryRequest {
    /// Raw Cypher statement. The tenant label is injected automatically; multi-statement
    /// queries (semicolons outside strings) are rejected.
    pub cypher: String,
    /// Named parameters bound into the query (`$key` → value).
    #[serde(default)]
    pub params: serde_json::Map<String, Value>,
    /// When `true` (default), the query is pre-flight checked for write operators
    /// (CREATE, MERGE, SET, DELETE, DETACH, REMOVE) and rejected with 400 if found.
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_read_only() -> bool {
    true
}

/// Response body for `POST /v1/graph/query`.
#[derive(Debug, Serialize, ToSchema)]
pub struct GraphQueryResponse {
    /// Each element is a JSON object mapping column names to their values.
    pub rows: Vec<Value>,
    /// Number of rows returned.
    pub row_count: usize,
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

struct AdminAccess {
    tenant_id: Uuid,
}

/// Accept API keys with the `admin` scope **or** verified sessions whose tenant
/// role is `owner` or `admin`.
async fn require_admin_access(
    pool: &sqlx::PgPool,
    auth: AuthContext,
) -> Result<AdminAccess, AppError> {
    match auth {
        AuthContext::ApiKey(info) => {
            if info.scopes.contains(&Scope::Admin) {
                Ok(AdminAccess { tenant_id: info.tenant_id })
            } else {
                Err(AppError::InsufficientScope)
            }
        }
        other => {
            let session = require_verified_session(other)?;
            let row: Option<(String,)> = sqlx::query_as(
                "SELECT role FROM control.tenant_members \
                 WHERE tenant_id = $1 AND user_id = $2",
            )
            .bind(session.tenant_id)
            .bind(session.user_id)
            .fetch_optional(pool)
            .await?;

            match row {
                None => Err(AppError::NotAMember),
                Some((role,)) if role == "owner" || role == "admin" => {
                    Ok(AdminAccess { tenant_id: session.tenant_id })
                }
                Some(_) => Err(AppError::InsufficientRole),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Execute an arbitrary Cypher query against the tenant's Neo4j graph store.
///
/// The tenant label is automatically injected into every node pattern so queries
/// are isolated to the calling tenant's data. Multi-statement queries
/// (containing a bare semicolon outside strings/comments) are rejected.
///
/// When `read_only` is `true` (the default), the query is pre-flight checked
/// for Cypher write operators (`CREATE`, `MERGE`, `SET`, `DELETE`, `DETACH`,
/// `REMOVE`). Any match causes a `400 cypher_write_denied` response before the
/// query reaches Neo4j.
///
/// Requires an API key with the `admin` scope **or** an active session with the
/// `owner` or `admin` tenant role.
#[utoipa::path(
    post,
    path = "/v1/graph/query",
    request_body = GraphQueryRequest,
    responses(
        (status = 200, description = "Query executed; rows returned", body = GraphQueryResponse),
        (status = 400, description = "Malformed query or write operators detected in read-only mode (cypher_write_denied / invalid_input)"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Insufficient role or scope"),
        (status = 503, description = "Neo4j graph store not configured (graph_not_configured)"),
    ),
    tag = "query"
)]
pub async fn post_graph_query(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(req): Json<GraphQueryRequest>,
) -> Result<impl IntoResponse, AppError> {
    let access = require_admin_access(&state.pool, auth).await?;

    let graph = state.graph.as_ref().ok_or(AppError::GraphNotConfigured)?;

    if req.read_only && has_write_operators(&req.cypher) {
        return Err(AppError::CypherWriteDenied);
    }

    let tenant_id = TenantId::from(access.tenant_id);
    let rows = graph.execute_query(&tenant_id, &req.cypher, &req.params).await?;
    let row_count = rows.len();

    Ok(Json(GraphQueryResponse { rows, row_count }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::ApiKeyInfo;

    fn admin_key(tenant_id: Uuid) -> ApiKeyInfo {
        ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        }
    }

    fn read_key(tenant_id: Uuid) -> ApiKeyInfo {
        ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        }
    }

    // ----- require_admin_access (unit, no DB) -----
    // We can only test the API-key branch synchronously; the session branch
    // needs a pool so is covered by integration tests.

    // ----- default_read_only -----

    #[test]
    fn read_only_defaults_to_true() {
        let req: GraphQueryRequest =
            serde_json::from_str(r#"{"cypher":"MATCH (n) RETURN n"}"#).unwrap();
        assert!(req.read_only);
    }

    #[test]
    fn read_only_can_be_set_false() {
        let req: GraphQueryRequest =
            serde_json::from_str(r#"{"cypher":"MATCH (n) RETURN n","read_only":false}"#).unwrap();
        assert!(!req.read_only);
    }

    #[test]
    fn params_defaults_to_empty_map() {
        let req: GraphQueryRequest =
            serde_json::from_str(r#"{"cypher":"MATCH (n) RETURN n"}"#).unwrap();
        assert!(req.params.is_empty());
    }

    // ----- GraphNotConfigured mapping -----

    #[test]
    fn graph_not_configured_returns_503() {
        let err = AppError::GraphNotConfigured;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    // ----- CypherWriteDenied mapping -----

    #[test]
    fn cypher_write_denied_returns_400() {
        let err = AppError::CypherWriteDenied;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    // ----- Response serialisation -----

    #[test]
    fn graph_query_response_serialises() {
        let resp = GraphQueryResponse {
            rows: vec![serde_json::json!({"id": 1, "name": "Foo"})],
            row_count: 1,
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["row_count"], 1);
        assert_eq!(val["rows"][0]["name"], "Foo");
    }

    // ----- write-denied integration with has_write_operators -----

    #[test]
    fn write_operators_trigger_write_denied_error() {
        let cypher = "CREATE (n:Foo) RETURN n";
        let read_only = true;
        let would_deny = read_only && has_write_operators(cypher);
        assert!(would_deny, "CREATE must be caught by write-denied guard");
    }

    #[test]
    fn read_query_with_read_only_passes_guard() {
        let cypher = "MATCH (n) RETURN n LIMIT 10";
        let read_only = true;
        let would_deny = read_only && has_write_operators(cypher);
        assert!(!would_deny);
    }

    #[test]
    fn write_query_with_read_only_false_passes_guard() {
        let cypher = "CREATE (n:Foo) RETURN n";
        let read_only = false;
        let would_deny = read_only && has_write_operators(cypher);
        assert!(!would_deny, "read_only=false must bypass the write-denied guard");
    }

    // ----- Admin scope check (no DB needed) -----

    #[test]
    fn admin_scope_not_in_read_key() {
        let key = read_key(Uuid::new_v4());
        assert!(!key.scopes.contains(&Scope::Admin));
    }

    #[test]
    fn admin_scope_in_admin_key() {
        let key = admin_key(Uuid::new_v4());
        assert!(key.scopes.contains(&Scope::Admin));
    }
}
