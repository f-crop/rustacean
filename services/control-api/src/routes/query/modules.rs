//! `GET /v1/repos/{repo_id}/modules` — Module tree (REQ-DP-06, RUSAA-77).
//!
//! Returns the crate/module hierarchy derived from `code_symbols.fqn`.
//! Fetches with one SQL, builds in-Rust, and caches the result in-process
//! for 60 s keyed by `(repo_id, last_ingest_run_id)` (ADR-008 §3.6 / §12.6).

use std::sync::Arc;

use axum::{Json, extract::State, response::IntoResponse};
use rb_schemas::TenantId;
use rb_tenant::TenantCtx;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_read_auth},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Response types (utoipa-annotated mirror of rb_query::ModuleNode)
// ---------------------------------------------------------------------------

/// Source location for a symbol.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct NodeSource {
    /// Relative path within the repository (e.g. `"src/lib.rs"`).
    pub path: String,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
}

/// A single node in the module/item tree.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModuleNodeItem {
    /// Leaf segment of the FQN (e.g. `"Vec"` for `alloc::vec::Vec`).
    pub name: String,
    /// Fully-qualified name (e.g. `"alloc::vec::Vec"`).
    pub fqn: String,
    /// Symbol kind from `code_symbols.kind` (e.g. `"MOD"`, `"STRUCT"`, `"FN"`).
    pub kind: String,
    /// Source location — absent for virtual/synthetic module nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<NodeSource>,
    /// Nested child nodes.
    #[schema(no_recursion)]
    pub children: Vec<ModuleNodeItem>,
}

/// Response envelope for `GET /v1/repos/{repo_id}/modules`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ModuleTreeResponse {
    pub repo_id: Uuid,
    pub tree: ModuleNodeItem,
}

// ---------------------------------------------------------------------------
// Conversion from rb_query::ModuleNode
// ---------------------------------------------------------------------------

impl From<rb_query::ModuleNode> for ModuleNodeItem {
    fn from(n: rb_query::ModuleNode) -> Self {
        let source = n.source_path.map(|path| NodeSource {
            path,
            line_start: n.line_start,
            line_end: n.line_end,
        });
        Self {
            name: n.name,
            fqn: n.fqn,
            kind: n.kind,
            source,
            children: n.children.into_iter().map(Self::from).collect(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Return the crate/module hierarchy for a repository.
///
/// Tree is derived from `code_symbols.fqn` (split on `::`) via a single SQL
/// query. Results are cached in-process for 60 s per `(repo_id, last_ingest_run_id)`.
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/modules",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID")
    ),
    responses(
        (status = 200, description = "Module tree for the repository", body = ModuleTreeResponse),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
    ),
    tag = "query"
)]
pub async fn get_module_tree(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path(repo_id): axum::extract::Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read_auth(auth)?;

    // Verify the repo belongs to this tenant (tenant isolation).
    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    exists.ok_or(AppError::NotFound)?;

    // Derive the cache key: (repo_id, last_succeeded_run_id).
    // Uuid::nil() when no run has completed yet — still worth caching to
    // avoid repeated empty-tree builds on a freshly connected repo.
    let run_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM control.ingestion_runs \
         WHERE repo_id = $1 AND tenant_id = $2 AND status = 'succeeded' \
         ORDER BY created_at DESC \
         LIMIT 1",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?
    .unwrap_or_else(Uuid::nil);

    let cache_key = (repo_id, run_id);

    // --- Cache hit path (AC3 / AC4 p95 ≤ 50 ms) ---
    if let Some(cached) = state.module_tree_cache.get(&cache_key).await {
        let resp = ModuleTreeResponse {
            repo_id,
            tree: ModuleNodeItem::from((*cached).clone()),
        };
        return Ok(Json(resp));
    }

    // --- Cache miss: fetch from DB and build tree (AC2 / AC4 cold ≤ 200 ms) ---
    let tenant_ctx = TenantCtx::new(TenantId::from(tenant_id));
    let node = rb_query::fetch_module_tree(&state.pool, &tenant_ctx, repo_id).await?;
    let node = Arc::new(node);

    state.module_tree_cache.insert(cache_key, Arc::clone(&node)).await;

    let resp = ModuleTreeResponse {
        repo_id,
        tree: ModuleNodeItem::from((*node).clone()),
    };
    Ok(Json(resp))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, Scope, SessionInfo};

    fn verified_session(tenant_id: Uuid) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        }
    }

    #[test]
    fn require_read_auth_accepts_verified_session() {
        let tenant_id = Uuid::new_v4();
        let auth = AuthContext::Session(verified_session(tenant_id));
        let result = require_read_auth(auth);
        assert_eq!(result.unwrap(), tenant_id);
    }

    #[test]
    fn require_read_auth_accepts_api_key_read_scope() {
        let tenant_id = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        assert_eq!(require_read_auth(AuthContext::ApiKey(key)).unwrap(), tenant_id);
    }

    #[test]
    fn require_read_auth_accepts_api_key_admin_scope() {
        let tenant_id = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        };
        assert_eq!(require_read_auth(AuthContext::ApiKey(key)).unwrap(), tenant_id);
    }

    #[test]
    fn require_read_auth_rejects_anonymous() {
        assert!(matches!(
            require_read_auth(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn require_read_auth_rejects_expired_session() {
        assert!(matches!(
            require_read_auth(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn require_read_auth_rejects_unverified_email() {
        let mut info = verified_session(Uuid::new_v4());
        info.email_verified = false;
        assert!(matches!(
            require_read_auth(AuthContext::Session(info)),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn module_node_item_from_rb_query_node_converts_source() {
        let node = rb_query::ModuleNode {
            name: "push".to_owned(),
            fqn: "alloc::vec::Vec::push".to_owned(),
            kind: "FN".to_owned(),
            source_path: Some("src/vec.rs".to_owned()),
            line_start: Some(1234),
            line_end: Some(1240),
            children: vec![],
        };
        let item = ModuleNodeItem::from(node);
        assert_eq!(item.name, "push");
        assert_eq!(item.kind, "FN");
        let src = item.source.unwrap();
        assert_eq!(src.path, "src/vec.rs");
        assert_eq!(src.line_start, Some(1234));
        assert_eq!(src.line_end, Some(1240));
        assert!(item.children.is_empty());
    }

    #[test]
    fn module_node_item_from_mod_node_has_no_source() {
        let node = rb_query::ModuleNode {
            name: "vec".to_owned(),
            fqn: "alloc::vec".to_owned(),
            kind: "MOD".to_owned(),
            source_path: None,
            line_start: None,
            line_end: None,
            children: vec![],
        };
        let item = ModuleNodeItem::from(node);
        assert!(item.source.is_none());
    }

    #[test]
    fn module_tree_response_serializes_repo_id_and_tree() {
        let repo_id = Uuid::new_v4();
        let resp = ModuleTreeResponse {
            repo_id,
            tree: ModuleNodeItem {
                name: "mycrate".to_owned(),
                fqn: "mycrate".to_owned(),
                kind: "MOD".to_owned(),
                source: None,
                children: vec![],
            },
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val.get("repo_id").is_some());
        assert_eq!(val["tree"]["name"], "mycrate");
        assert_eq!(val["tree"]["kind"], "MOD");
        assert!(val["tree"]["children"].is_array());
        // `source` is absent (skip_serializing_if = None)
        assert!(val["tree"].get("source").is_none());
    }
}
