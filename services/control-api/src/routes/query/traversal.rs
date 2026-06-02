//! `GET /v1/repos/{repo_id}/items/{fqn_b64}/callers` and `.../callees` (REQ-DP-03 / ADR-008 §3.3).
//!
//! BFS traversal of the Neo4j call graph within a tenant.  Depth-limited (default 3, max 10).
//! Results are paginated via an opaque base64 offset cursor.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rb_query::{
    DEFAULT_DEPTH, DEFAULT_LIMIT, EdgeProvenance, MAX_DEPTH, MAX_LIMIT, TraversalEdge,
    TraversalNode, TraversalOptions, TraversalResult, fetch_callees, fetch_callers,
};
use rb_schemas::TenantId;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

fn require_read_access(auth: AuthContext) -> Result<Uuid, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) if info.scopes.contains(&Scope::Read) => Ok(info.tenant_id),
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::McpJwt(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// Response types (re-exported from rb-query, wrapped for utoipa)
// ---------------------------------------------------------------------------

/// Per-edge dispatch provenance.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EdgeProvenanceSchema {
    Direct,
    Monomorph,
    DynCandidate,
}

impl From<EdgeProvenance> for EdgeProvenanceSchema {
    fn from(p: EdgeProvenance) -> Self {
        match p {
            EdgeProvenance::Direct => Self::Direct,
            EdgeProvenance::Monomorph => Self::Monomorph,
            EdgeProvenance::DynCandidate => Self::DynCandidate,
        }
    }
}

/// A node discovered during BFS traversal.
#[derive(Debug, Serialize, ToSchema)]
pub struct TraversalNodeSchema {
    pub fqn: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<i64>,
}

impl From<TraversalNode> for TraversalNodeSchema {
    fn from(n: TraversalNode) -> Self {
        Self {
            fqn: n.fqn,
            name: n.name,
            kind: n.kind,
            file_path: n.file_path,
            line: n.line,
        }
    }
}

/// A directed edge in the call graph.
#[derive(Debug, Serialize, ToSchema)]
pub struct TraversalEdgeSchema {
    pub from_fqn: String,
    pub to_fqn: String,
    pub depth: u32,
    pub provenance: EdgeProvenanceSchema,
}

impl From<TraversalEdge> for TraversalEdgeSchema {
    fn from(e: TraversalEdge) -> Self {
        Self {
            from_fqn: e.from_fqn,
            to_fqn: e.to_fqn,
            depth: e.depth,
            provenance: e.provenance.into(),
        }
    }
}

/// Response for caller/callee traversal endpoints.
#[derive(Debug, Serialize, ToSchema)]
pub struct TraversalResponse {
    pub root: TraversalNodeSchema,
    pub nodes: Vec<TraversalNodeSchema>,
    pub edges: Vec<TraversalEdgeSchema>,
    pub cycles_detected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

impl From<TraversalResult> for TraversalResponse {
    fn from(r: TraversalResult) -> Self {
        Self {
            root: r.root.into(),
            nodes: r.nodes.into_iter().map(Into::into).collect(),
            edges: r.edges.into_iter().map(Into::into).collect(),
            cycles_detected: r.cycles_detected,
            next_cursor: r.next_cursor,
        }
    }
}

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

fn default_depth() -> u32 {
    DEFAULT_DEPTH
}
fn default_limit() -> usize {
    DEFAULT_LIMIT
}

/// Query parameters for BFS traversal endpoints.
#[derive(Debug, Deserialize, IntoParams)]
pub struct TraversalQuery {
    /// BFS traversal depth (1–10, default 3).
    #[serde(default = "default_depth")]
    pub depth: u32,
    /// Maximum edges to return per page (1–200, default 50).
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Opaque continuation cursor from a prior response.
    pub cursor: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared validation + options builder
// ---------------------------------------------------------------------------

fn build_opts(q: TraversalQuery) -> Result<TraversalOptions, AppError> {
    let TraversalQuery {
        depth,
        limit,
        cursor,
    } = q;
    if depth > MAX_DEPTH {
        return Err(AppError::InvalidInput);
    }
    if limit > MAX_LIMIT || limit == 0 {
        return Err(AppError::InvalidInput);
    }
    let offset = match cursor {
        Some(c) => {
            let bytes = URL_SAFE_NO_PAD
                .decode(c.as_bytes())
                .map_err(|_| AppError::InvalidInput)?;
            let s = String::from_utf8(bytes).map_err(|_| AppError::InvalidInput)?;
            s.parse::<usize>().map_err(|_| AppError::InvalidInput)?
        }
        None => 0,
    };
    Ok(TraversalOptions {
        depth,
        limit,
        offset,
    })
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// List all functions that transitively call the target item (callers BFS).
///
/// Traverses `CALLS` and `CALL_INSTANTIATES` edges backward from the root.
/// Cycle detection prevents infinite loops; per-edge provenance distinguishes
/// static (`direct`), monomorphized (`monomorph`), and dynamic (`dyn_candidate`) calls.
/// Use `next_cursor` for pagination.
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/items/{fqn_b64}/callers",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID"),
        ("fqn_b64" = String, Path, description = "URL-safe base64 (no padding) encoded item FQN"),
        TraversalQuery,
    ),
    responses(
        (status = 200, description = "Caller graph", body = TraversalResponse),
        (status = 400, description = "Malformed fqn_b64, depth > 10, or invalid cursor"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
        (status = 503, description = "Neo4j graph not configured on this instance"),
    ),
    tag = "query"
)]
pub async fn get_callers(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((repo_id, fqn_b64)): Path<(Uuid, String)>,
    Query(query): Query<TraversalQuery>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read_access(auth)?;
    let graph = state.graph.as_deref().ok_or(AppError::GraphUnavailable)?;

    let fqn_bytes = URL_SAFE_NO_PAD
        .decode(fqn_b64.as_bytes())
        .map_err(|_| AppError::InvalidInput)?;
    let fqn = String::from_utf8(fqn_bytes).map_err(|_| AppError::InvalidInput)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let opts = build_opts(query)?;
    let tid = TenantId::from(tenant_id);

    let result = fetch_callers(graph, &tid, repo_id, &fqn, opts).await?;

    tracing::debug!(
        %repo_id,
        fqn = %fqn,
        edge_count = result.edges.len(),
        tenant_id = %tenant_id,
        "callers traversal"
    );

    Ok(Json(TraversalResponse::from(result)))
}

/// List all functions transitively called by the target item (callees BFS).
///
/// Traverses `CALLS` and `CALL_INSTANTIATES` edges forward from the root.
/// See the callers endpoint for provenance semantics and pagination.
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/items/{fqn_b64}/callees",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID"),
        ("fqn_b64" = String, Path, description = "URL-safe base64 (no padding) encoded item FQN"),
        TraversalQuery,
    ),
    responses(
        (status = 200, description = "Callee graph", body = TraversalResponse),
        (status = 400, description = "Malformed fqn_b64, depth > 10, or invalid cursor"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
        (status = 503, description = "Neo4j graph not configured on this instance"),
    ),
    tag = "query"
)]
pub async fn get_callees(
    State(state): State<AppState>,
    auth: AuthContext,
    Path((repo_id, fqn_b64)): Path<(Uuid, String)>,
    Query(query): Query<TraversalQuery>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read_access(auth)?;
    let graph = state.graph.as_deref().ok_or(AppError::GraphUnavailable)?;

    let fqn_bytes = URL_SAFE_NO_PAD
        .decode(fqn_b64.as_bytes())
        .map_err(|_| AppError::InvalidInput)?;
    let fqn = String::from_utf8(fqn_bytes).map_err(|_| AppError::InvalidInput)?;

    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let opts = build_opts(query)?;
    let tid = TenantId::from(tenant_id);

    let result = fetch_callees(graph, &tid, repo_id, &fqn, opts).await?;

    tracing::debug!(
        %repo_id,
        fqn = %fqn,
        edge_count = result.edges.len(),
        tenant_id = %tenant_id,
        "callees traversal"
    );

    Ok(Json(TraversalResponse::from(result)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session(tid: Uuid) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: tid,
            email_verified: true,
        }
    }

    #[test]
    fn require_read_access_accepts_verified_session() {
        let tid = Uuid::new_v4();
        assert_eq!(
            require_read_access(AuthContext::Session(verified_session(tid))).unwrap(),
            tid
        );
    }

    #[test]
    fn require_read_access_accepts_read_api_key() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        assert_eq!(require_read_access(AuthContext::ApiKey(key)).unwrap(), tid);
    }

    #[test]
    fn require_read_access_rejects_unverified_session() {
        let mut info = verified_session(Uuid::new_v4());
        info.email_verified = false;
        assert!(matches!(
            require_read_access(AuthContext::Session(info)),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn require_read_access_rejects_anonymous() {
        assert!(matches!(
            require_read_access(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn build_opts_defaults() {
        let q = TraversalQuery {
            depth: DEFAULT_DEPTH,
            limit: DEFAULT_LIMIT,
            cursor: None,
        };
        let opts = build_opts(q).unwrap();
        assert_eq!(opts.depth, DEFAULT_DEPTH);
        assert_eq!(opts.limit, DEFAULT_LIMIT);
        assert_eq!(opts.offset, 0);
    }

    #[test]
    fn build_opts_rejects_depth_over_max() {
        let q = TraversalQuery {
            depth: MAX_DEPTH + 1,
            limit: DEFAULT_LIMIT,
            cursor: None,
        };
        assert!(matches!(build_opts(q), Err(AppError::InvalidInput)));
    }

    #[test]
    fn build_opts_rejects_limit_zero() {
        let q = TraversalQuery {
            depth: DEFAULT_DEPTH,
            limit: 0,
            cursor: None,
        };
        assert!(matches!(build_opts(q), Err(AppError::InvalidInput)));
    }

    #[test]
    fn build_opts_rejects_limit_over_max() {
        let q = TraversalQuery {
            depth: DEFAULT_DEPTH,
            limit: MAX_LIMIT + 1,
            cursor: None,
        };
        assert!(matches!(build_opts(q), Err(AppError::InvalidInput)));
    }

    #[test]
    fn build_opts_decodes_valid_cursor() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let cursor = URL_SAFE_NO_PAD.encode(b"42");
        let q = TraversalQuery {
            depth: DEFAULT_DEPTH,
            limit: DEFAULT_LIMIT,
            cursor: Some(cursor),
        };
        let opts = build_opts(q).unwrap();
        assert_eq!(opts.offset, 42);
    }

    #[test]
    fn build_opts_rejects_invalid_cursor() {
        let q = TraversalQuery {
            depth: DEFAULT_DEPTH,
            limit: DEFAULT_LIMIT,
            cursor: Some("!!!bad!!!".into()),
        };
        assert!(matches!(build_opts(q), Err(AppError::InvalidInput)));
    }

    #[test]
    fn fqn_b64_roundtrip() {
        let fqn = "my_crate::module::my_function";
        let encoded = URL_SAFE_NO_PAD.encode(fqn.as_bytes());
        let decoded = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), fqn);
    }

    #[test]
    fn traversal_response_from_result() {
        let result = TraversalResult {
            root: TraversalNode {
                fqn: "root::fn".into(),
                name: Some("fn".into()),
                kind: None,
                file_path: None,
                line: None,
            },
            nodes: vec![],
            edges: vec![],
            cycles_detected: false,
            next_cursor: None,
        };
        let resp = TraversalResponse::from(result);
        assert_eq!(resp.root.fqn, "root::fn");
        assert!(!resp.cycles_detected);
    }
}
