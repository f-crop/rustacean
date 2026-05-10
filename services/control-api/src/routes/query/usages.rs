//! `GET /v1/repos/{repo_id}/items/{fqn_b64}/usages` — type usage lookup (REQ-DP-04 / ADR-008 §3.4).
//!
//! Returns both textual usages (`USES_TYPE` edges) and monomorphized instances
//! (`MONOMORPHIZED_FROM` edges from `TypeInstance` nodes).  The response
//! distinguishes the two by the `usage_kind` field.

use axum::{Json, extract::State, response::IntoResponse};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rb_query::fetch_type_usages;
use rb_schemas::TenantId;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, require_read_auth},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Response schema
// ---------------------------------------------------------------------------

/// One item that references the queried type.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageEntry {
    /// Fully-qualified name of the using item or type instance.
    pub fqn: String,
    /// `"textual"` — item uses the type by name in its source.
    /// `"monomorphized"` — `TypeInstance` instantiated from this type's `TypeDef`.
    pub usage_kind: String,
}

/// Response for `GET /v1/repos/{repo_id}/items/{fqn_b64}/usages`.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsagesResponse {
    /// Repository UUID.
    pub repo_id: Uuid,
    /// FQN of the queried type.
    pub type_fqn: String,
    /// All usages found in the graph, combining textual and monomorphized.
    pub usages: Vec<UsageEntry>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// List all usages of the type identified by `fqn_b64` within a repository.
///
/// Returns two categories of usage combined in a flat list:
/// - **Textual** (`usage_kind = "textual"`)  — items that reference the type
///   by name (`USES_TYPE` edge).
/// - **Monomorphized** (`usage_kind = "monomorphized"`) — `TypeInstance` nodes
///   derived from this type via a `MONOMORPHIZED_FROM` edge.
///
/// `fqn_b64` must be URL-safe base64 (no padding) of the type's FQN.
/// Requires the graph to be configured (`RB_NEO4J_URI`); returns 503 otherwise.
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/items/{fqn_b64}/usages",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID"),
        ("fqn_b64" = String, Path, description = "URL-safe base64 (no padding) encoded type FQN"),
    ),
    responses(
        (status = 200, description = "Usage list", body = UsagesResponse),
        (status = 400, description = "Malformed fqn_b64"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
        (status = 503, description = "Neo4j graph not configured on this instance"),
    ),
    tag = "query"
)]
pub async fn get_type_usages(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path((repo_id, fqn_b64)): axum::extract::Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read_auth(auth)?;

    let graph = state.graph.as_deref().ok_or(AppError::GraphUnavailable)?;

    let fqn_bytes = URL_SAFE_NO_PAD
        .decode(fqn_b64.as_bytes())
        .map_err(|_| AppError::InvalidInput)?;
    let fqn = String::from_utf8(fqn_bytes).map_err(|_| AppError::InvalidInput)?;

    // Verify the repo belongs to this tenant.
    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    let tid = TenantId::from(tenant_id);
    let entries = fetch_type_usages(graph, &tid, repo_id, &fqn).await?;

    tracing::debug!(
        %repo_id,
        fqn = %fqn,
        count = entries.len(),
        tenant_id = %tenant_id,
        "type usages lookup"
    );

    Ok(Json(UsagesResponse {
        repo_id,
        type_fqn: fqn,
        usages: entries
            .into_iter()
            .map(|e| UsageEntry {
                fqn: e.fqn,
                usage_kind: e.usage_kind,
            })
            .collect(),
    }))
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
        let tid = Uuid::new_v4();
        let result = require_read_auth(AuthContext::Session(verified_session(tid)));
        assert_eq!(result.unwrap(), tid);
    }

    #[test]
    fn require_read_auth_accepts_api_key_any_scope() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        };
        assert_eq!(require_read_auth(AuthContext::ApiKey(key)).unwrap(), tid);
    }

    #[test]
    fn require_read_auth_rejects_expired_session() {
        assert!(matches!(
            require_read_auth(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn valid_fqn_b64_roundtrips() {
        let fqn = "std::vec::Vec";
        let encoded = URL_SAFE_NO_PAD.encode(fqn.as_bytes());
        let decoded = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), fqn);
    }

    #[test]
    fn usages_response_serializes_correctly() {
        let resp = UsagesResponse {
            repo_id: Uuid::new_v4(),
            type_fqn: "my_crate::MyType".to_owned(),
            usages: vec![
                UsageEntry {
                    fqn: "my_crate::uses_it".to_owned(),
                    usage_kind: "textual".to_owned(),
                },
                UsageEntry {
                    fqn: "my_crate::MyType<i32>".to_owned(),
                    usage_kind: "monomorphized".to_owned(),
                },
            ],
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["type_fqn"], "my_crate::MyType");
        let usages = val["usages"].as_array().unwrap();
        assert_eq!(usages.len(), 2);
        assert_eq!(usages[0]["usage_kind"], "textual");
        assert_eq!(usages[1]["usage_kind"], "monomorphized");
    }

    #[test]
    fn empty_usages_serializes_correctly() {
        let resp = UsagesResponse {
            repo_id: Uuid::new_v4(),
            type_fqn: "my_crate::Unused".to_owned(),
            usages: vec![],
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val["usages"].as_array().unwrap().is_empty());
    }

    #[test]
    fn graph_unavailable_returns_503() {
        let err = AppError::GraphUnavailable;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
