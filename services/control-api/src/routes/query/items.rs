//! `GET /v1/repos/{repo_id}/items/{fqn_b64}` — item lookup (REQ-DP-02 / ADR-008 §12.2).
//!
//! `fqn_b64` is the fully-qualified name encoded as URL-safe base64 (no padding).
//! Accepts both verified session cookies and API keys with the `read` scope.

use axum::{Json, extract::State, response::IntoResponse};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rb_query::items;
use rb_schemas::TenantId;
use rb_tenant::TenantCtx;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Response schema (ADR-008 §3.2)
// ---------------------------------------------------------------------------

/// Code symbol returned by the item-lookup endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct ItemResponse {
    /// Internal symbol UUID.
    pub id: Uuid,
    /// Fully-qualified name (e.g. `my_crate::module::MyStruct`).
    pub fqn: String,
    /// `ItemKind` string (e.g. `"FN"`, `"STRUCT"`, `"TRAIT"`).
    pub kind: String,
    /// Repository this symbol belongs to.
    pub repo_id: Uuid,
    /// Repo-relative source path (e.g. `src/lib.rs`).
    pub source_path: Option<String>,
    /// 1-based start line of the item in `source_path`.
    pub line_start: Option<i32>,
    /// 1-based end line of the item in `source_path`.
    pub line_end: Option<i32>,
    /// Inline source text — present when the item's source is ≤ 4 KiB.
    /// Absent when `blob_ref` is populated (AC3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_preview: Option<String>,
    /// `rb-blob://` URI for the item's serialised AST JSON.
    /// Populated only when the item's source exceeds 4 KiB (AC3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob_ref: Option<String>,
}

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

/// Tenant identity extracted from either a verified session or a read-scoped
/// API key.
struct ReadAccess {
    tenant_id: Uuid,
}

/// Accept verified sessions **and** API keys with the `read` scope.
///
/// Policy: the item-lookup endpoint is a pure read; programmatic callers
/// should be able to use API keys without needing a browser session.
fn require_read_access(auth: AuthContext) -> Result<ReadAccess, AppError> {
    match auth {
        AuthContext::Session(info) if info.email_verified => Ok(ReadAccess {
            tenant_id: info.tenant_id,
        }),
        AuthContext::Session(_) => Err(AppError::EmailNotVerified),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(info) if info.scopes.contains(&Scope::Read) => Ok(ReadAccess {
            tenant_id: info.tenant_id,
        }),
        AuthContext::ApiKey(_) => Err(AppError::InsufficientScope),
        AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// Retrieve a code symbol by its fully-qualified name within a repository.
///
/// `fqn_b64` must be the FQN encoded as URL-safe base64 without padding
/// (`base64url` / RFC 4648 §5, no `=` padding). Returns the item's metadata
/// and, when the stored source is ≤ 4 KiB, an inline `source_preview`.
/// Larger items carry only a `blob_ref` URI pointing to the full AST JSON.
///
/// Cross-tenant requests are rejected: the repository must belong to the
/// caller's tenant (AC4). Returns 404 both when the repo is absent and when
/// the `(repo_id, fqn)` tuple is not found (AC2).
#[utoipa::path(
    get,
    path = "/v1/repos/{repo_id}/items/{fqn_b64}",
    params(
        ("repo_id" = Uuid, Path, description = "Repository UUID (from POST /v1/repos)"),
        ("fqn_b64" = String, Path, description = "URL-safe base64 (no padding) encoded fully-qualified name"),
    ),
    responses(
        (status = 200, description = "Item found", body = ItemResponse),
        (status = 400, description = "Malformed fqn_b64 encoding (invalid_input)"),
        (status = 401, description = "Not authenticated or session expired"),
        (status = 403, description = "Email not verified or API key lacks read scope"),
        (status = 404, description = "Repository or item not found (not_found)"),
    ),
    tag = "query"
)]
pub async fn get_item(
    State(state): State<AppState>,
    auth: AuthContext,
    axum::extract::Path((repo_id, fqn_b64)): axum::extract::Path<(Uuid, String)>,
) -> Result<impl IntoResponse, AppError> {
    let access = require_read_access(auth)?;

    // Decode the URL-safe base64 FQN (AC1: fqn_b64 path segment).
    let fqn_bytes = URL_SAFE_NO_PAD
        .decode(fqn_b64.as_bytes())
        .map_err(|_| AppError::InvalidInput)?;
    let fqn = String::from_utf8(fqn_bytes).map_err(|_| AppError::InvalidInput)?;

    // AC4: Verify the repo belongs to this tenant before touching the tenant schema.
    let owned: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM control.repos \
         WHERE id = $1 AND tenant_id = $2 AND archived_at IS NULL",
    )
    .bind(repo_id)
    .bind(access.tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    owned.ok_or(AppError::NotFound)?;

    // Build a TenantCtx for fully-qualified table-name resolution.
    let tenant_ctx = TenantCtx::new(TenantId::from(access.tenant_id));

    // AC1 + AC2: fetch symbol; return 404 when absent.
    let symbol = items::get_by_fqn(&state.pool, &tenant_ctx, repo_id, &fqn)
        .await?
        .ok_or(AppError::NotFound)?;

    // AC3: source_preview from source_text column for inline items (≤ 512 KiB);
    // blob_ref for large items whose source was stored in the blob store.
    let (source_preview, blob_ref) = match symbol.blob_ref {
        Some(r) => (None, Some(r)),
        None => (symbol.source_text, None),
    };

    tracing::debug!(
        %repo_id,
        fqn = %fqn,
        kind = %symbol.kind,
        tenant_id = %access.tenant_id,
        "item lookup"
    );

    Ok(Json(ItemResponse {
        id: symbol.id,
        fqn: symbol.fqn,
        kind: symbol.kind,
        repo_id,
        source_path: symbol.source_path,
        line_start: symbol.line_start,
        line_end: symbol.line_end,
        source_preview,
        blob_ref,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn verified_session(tenant_id: Uuid) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id,
            email_verified: true,
        }
    }

    // ----- require_read_access -----

    #[test]
    fn anonymous_is_rejected() {
        assert!(matches!(
            require_read_access(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn expired_session_is_rejected() {
        assert!(matches!(
            require_read_access(AuthContext::ExpiredSession),
            Err(AppError::SessionExpired)
        ));
    }

    #[test]
    fn unverified_session_is_rejected() {
        let mut info = verified_session(Uuid::new_v4());
        info.email_verified = false;
        assert!(matches!(
            require_read_access(AuthContext::Session(info)),
            Err(AppError::EmailNotVerified)
        ));
    }

    #[test]
    fn verified_session_is_accepted() {
        let tid = Uuid::new_v4();
        let access = require_read_access(AuthContext::Session(verified_session(tid))).unwrap();
        assert_eq!(access.tenant_id, tid);
    }

    #[test]
    fn api_key_with_read_scope_accepted() {
        let tid = Uuid::new_v4();
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: tid,
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Read],
        };
        let access = require_read_access(AuthContext::ApiKey(key)).unwrap();
        assert_eq!(access.tenant_id, tid);
    }

    #[test]
    fn api_key_without_read_scope_rejected() {
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Write],
        };
        assert!(matches!(
            require_read_access(AuthContext::ApiKey(key)),
            Err(AppError::InsufficientScope)
        ));
    }

    #[test]
    fn api_key_with_admin_scope_rejected() {
        // Admin scope alone is not read — must explicitly have read.
        let key = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        };
        assert!(matches!(
            require_read_access(AuthContext::ApiKey(key)),
            Err(AppError::InsufficientScope)
        ));
    }

    // ----- fqn_b64 decoding -----

    #[test]
    fn valid_fqn_b64_roundtrips() {
        let fqn = "my_crate::module::MyStruct";
        let encoded = URL_SAFE_NO_PAD.encode(fqn.as_bytes());
        let decoded = URL_SAFE_NO_PAD.decode(encoded.as_bytes()).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), fqn);
    }

    #[test]
    fn invalid_base64_maps_to_invalid_input() {
        let err = URL_SAFE_NO_PAD
            .decode(b"not-valid-base64!@#")
            .map_err(|_| AppError::InvalidInput);
        assert!(matches!(err, Err(AppError::InvalidInput)));
    }

    // ----- response shape -----

    #[test]
    fn item_response_serializes_all_fields() {
        let resp = ItemResponse {
            id: Uuid::new_v4(),
            fqn: "my_crate::Foo".to_owned(),
            kind: "STRUCT".to_owned(),
            repo_id: Uuid::new_v4(),
            source_path: Some("src/lib.rs".to_owned()),
            line_start: Some(10),
            line_end: Some(20),
            source_preview: None,
            blob_ref: None,
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["kind"], "STRUCT");
        assert_eq!(val["fqn"], "my_crate::Foo");
        assert_eq!(val["line_start"], 10);
        assert_eq!(val["line_end"], 20);
        // skip_serializing_if = None fields should be absent
        assert!(val.get("source_preview").is_none());
        assert!(val.get("blob_ref").is_none());
    }

    #[test]
    fn blob_ref_present_when_source_large() {
        let resp = ItemResponse {
            id: Uuid::new_v4(),
            fqn: "my_crate::huge_fn".to_owned(),
            kind: "FN".to_owned(),
            repo_id: Uuid::new_v4(),
            source_path: Some("src/lib.rs".to_owned()),
            line_start: Some(1),
            line_end: Some(500),
            source_preview: None,
            blob_ref: Some("rb-blob://tenant/items/x.json".to_owned()),
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val.get("blob_ref").is_some());
        assert!(val.get("source_preview").is_none());
    }

    #[test]
    fn source_preview_present_when_source_text_populated() {
        let resp = ItemResponse {
            id: Uuid::new_v4(),
            fqn: "my_crate::foo".to_owned(),
            kind: "FN".to_owned(),
            repo_id: Uuid::new_v4(),
            source_path: Some("src/lib.rs".to_owned()),
            line_start: Some(10),
            line_end: Some(12),
            source_preview: Some("fn foo() {}".to_owned()),
            blob_ref: None,
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["source_preview"], "fn foo() {}");
        assert!(val.get("blob_ref").is_none());
    }

    #[test]
    fn source_preview_absent_when_no_source_text() {
        let resp = ItemResponse {
            id: Uuid::new_v4(),
            fqn: "my_crate::bar".to_owned(),
            kind: "FN".to_owned(),
            repo_id: Uuid::new_v4(),
            source_path: None,
            line_start: None,
            line_end: None,
            source_preview: None,
            blob_ref: None,
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val.get("source_preview").is_none());
        assert!(val.get("blob_ref").is_none());
    }

    #[test]
    fn not_found_returns_404() {
        let err = AppError::NotFound;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn invalid_input_returns_400() {
        let err = AppError::InvalidInput;
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::BAD_REQUEST);
    }
}
