//! `GET /v1/repos/{repo_id}/items/{fqn_b64}/impls` — trait impl lookup (REQ-DP-04 / ADR-008 §3.4).
//!
//! Returns direct and blanket impl blocks for the given trait FQN within a
//! repository.  Queries are tenant-isolated via `TenantGraph::execute_read`.

use axum::{Json, extract::State, response::IntoResponse};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rb_query::fetch_trait_impls;
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

/// One impl block that implements the queried trait.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImplEntry {
    /// Fully-qualified name of the impl block.
    pub fqn: String,
    /// `"direct"` for concrete impls; `"blanket"` for blanket impls.
    pub impl_kind: String,
}

/// Response for `GET /v1/repos/{repo_id}/items/{fqn_b64}/impls`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ImplsResponse {
    /// Repository UUID.
    pub repo_id: Uuid,
    /// FQN of the queried trait.
    pub trait_fqn: String,
    /// Impl blocks found in the graph.
    pub impls: Vec<ImplEntry>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// List all impl blocks for the trait identified by `fqn_b64` within a repository.
///
/// Returns both direct impls (`impl Trait for Type`) and blanket impls
/// (`impl<T: Bound> Trait for T`) found in the graph.
///
/// `fqn_b64` must be URL-safe base64 (no padding) of the trait's FQN.
/// Requires the graph to be configured (`RB_NEO4J_URI`); returns 503 otherwise.
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/items/{fqn_b64}/impls",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID"),
        ("fqn_b64" = String, Path, description = "URL-safe base64 (no padding) encoded trait FQN"),
    ),
    responses(
        (status = 200, description = "Impl list", body = ImplsResponse),
        (status = 400, description = "Malformed fqn_b64"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or insufficient scope"),
        (status = 404, description = "Repository not found or belongs to another tenant"),
        (status = 503, description = "Neo4j graph not configured on this instance"),
    ),
    tag = "query"
)]
pub async fn get_trait_impls(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path((repo_id, fqn_b64)): axum::extract::Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    let tenant_id = require_read_auth(auth)?;

    let graph = state.graph.as_deref().ok_or(AppError::GraphUnavailable)?;

    let fqn_bytes =
        URL_SAFE_NO_PAD.decode(fqn_b64.as_bytes()).map_err(|_| AppError::InvalidInput)?;
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
    let entries = fetch_trait_impls(graph, &tid, repo_id, &fqn).await?;

    tracing::debug!(
        %repo_id,
        fqn = %fqn,
        count = entries.len(),
        tenant_id = %tenant_id,
        "trait impls lookup"
    );

    Ok(Json(ImplsResponse {
        repo_id,
        trait_fqn: fqn,
        impls: entries
            .into_iter()
            .map(|e| ImplEntry { fqn: e.fqn, impl_kind: e.impl_kind })
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
    fn require_read_auth_accepts_api_key_read_scope() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        assert_eq!(require_read_auth(AuthContext::ApiKey(key)).unwrap(), tid);
    }

    #[test]
    fn require_read_auth_rejects_anonymous() {
        assert!(matches!(
            require_read_auth(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn valid_fqn_b64_roundtrips() {
        let fqn = "my_crate::module::MyTrait";
        let encoded = URL_SAFE_NO_PAD.encode(fqn.as_bytes());
        let decoded = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), fqn);
    }

    #[test]
    fn invalid_base64_maps_to_invalid_input() {
        let err = URL_SAFE_NO_PAD.decode(b"not-valid!@#").map_err(|_| AppError::InvalidInput);
        assert!(matches!(err, Err(AppError::InvalidInput)));
    }

    #[test]
    fn impls_response_serializes_correctly() {
        let resp = ImplsResponse {
            repo_id: Uuid::new_v4(),
            trait_fqn: "my_crate::MyTrait".to_owned(),
            impls: vec![
                ImplEntry { fqn: "my_crate::Foo".to_owned(), impl_kind: "direct".to_owned() },
                ImplEntry {
                    fqn: "my_crate::GenericFoo".to_owned(),
                    impl_kind: "blanket".to_owned(),
                },
            ],
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["trait_fqn"], "my_crate::MyTrait");
        let impls = val["impls"].as_array().unwrap();
        assert_eq!(impls.len(), 2);
        assert_eq!(impls[0]["impl_kind"], "direct");
        assert_eq!(impls[1]["impl_kind"], "blanket");
    }

    #[test]
    fn empty_impls_response_serializes_correctly() {
        let resp = ImplsResponse {
            repo_id: Uuid::new_v4(),
            trait_fqn: "my_crate::UnimplementedTrait".to_owned(),
            impls: vec![],
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val["impls"].as_array().unwrap().is_empty());
    }

    #[test]
    fn graph_unavailable_returns_503() {
        let err = AppError::GraphUnavailable;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }
}
