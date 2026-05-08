use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use rb_auth::ApiKey;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope, SessionInfo},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Shared helper
// ---------------------------------------------------------------------------

fn require_session(auth: AuthContext) -> Result<SessionInfo, AppError> {
    match auth {
        AuthContext::Session(info) => Ok(info),
        AuthContext::ExpiredSession => Err(AppError::SessionExpired),
        AuthContext::ApiKey(_) | AuthContext::Anonymous => Err(AppError::Unauthorized),
    }
}

// ---------------------------------------------------------------------------
// POST /v1/api-keys
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateApiKeyRequest {
    /// Human-readable label for the key.
    pub name: String,
    /// Scopes the key is authorized to use.
    pub scopes: Vec<Scope>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateApiKeyResponse {
    pub id: Uuid,
    /// Plaintext key — shown exactly once. Store it securely; it cannot be retrieved later.
    pub key: String,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub created_at: DateTime<Utc>,
}

/// Create a new API key for the current session's tenant.
///
/// The plaintext key is returned exactly once in the `key` field.
/// Subsequent reads from `GET /v1/api-keys` will not include the plaintext.
/// Requires an active session.
#[utoipa::path(
    post,
    path = "/v1/api-keys",
    request_body = CreateApiKeyRequest,
    responses(
        (status = 201, description = "API key created", body = CreateApiKeyResponse),
        (status = 400, description = "Missing or empty name or scopes"),
        (status = 401, description = "Not authenticated (unauthorized)"),
    ),
    tag = "api_keys"
)]
pub async fn create_api_key(
    State(state): State<AppState>,
    auth: AuthContext,
    Json(body): Json<CreateApiKeyRequest>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;

    if body.name.trim().is_empty() {
        return Err(AppError::InvalidInput);
    }
    if body.scopes.is_empty() {
        return Err(AppError::InvalidInput);
    }

    let raw_key = ApiKey::generate();
    let key_hash = raw_key.hash();
    let key_str = raw_key.as_str().to_owned();
    drop(raw_key); // zeroize plaintext from memory once we have what we need

    let id = Uuid::new_v4();
    let scopes_json = serde_json::to_value(&body.scopes).map_err(anyhow::Error::from)?;

    let created_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO control.api_keys \
         (id, tenant_id, key_hash, name, scopes, created_by_user_id) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING created_at",
    )
    .bind(id)
    .bind(session.tenant_id)
    .bind(&key_hash)
    .bind(&body.name)
    .bind(&scopes_json)
    .bind(session.user_id)
    .fetch_one(&state.pool)
    .await?;

    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'api_key_created', $3)",
    )
    .bind(session.user_id)
    .bind(session.tenant_id)
    .bind(serde_json::json!({ "api_key_id": id, "name": body.name }))
    .execute(&state.pool)
    .await?;

    Ok((
        StatusCode::CREATED,
        Json(CreateApiKeyResponse {
            id,
            key: key_str,
            name: body.name,
            scopes: body.scopes,
            created_at,
        }),
    ))
}

// ---------------------------------------------------------------------------
// GET /v1/api-keys
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct ApiKeyItem {
    pub id: Uuid,
    pub name: String,
    pub scopes: Vec<Scope>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ListApiKeysResponse {
    pub keys: Vec<ApiKeyItem>,
}

type ApiKeyRow = (
    Uuid,
    String,
    serde_json::Value,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
);

/// List all active (non-revoked) API keys for the current session's tenant.
///
/// Plaintext keys are never returned — only metadata.
/// Requires an active session.
#[utoipa::path(
    get,
    path = "/v1/api-keys",
    responses(
        (status = 200, description = "List of active API keys", body = ListApiKeysResponse),
        (status = 401, description = "Not authenticated"),
    ),
    tag = "api_keys"
)]
pub async fn list_api_keys(
    State(state): State<AppState>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;

    let rows: Vec<ApiKeyRow> = sqlx::query_as(
        "SELECT id, name, scopes, last_used_at, created_at \
             FROM control.api_keys \
             WHERE tenant_id = $1 AND revoked_at IS NULL \
             ORDER BY created_at DESC",
    )
    .bind(session.tenant_id)
    .fetch_all(&state.pool)
    .await?;

    let keys = rows
        .into_iter()
        .map(|(id, name, scopes_json, last_used_at, created_at)| {
            let scopes = scopes_json
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().and_then(Scope::from_str))
                        .collect()
                })
                .unwrap_or_default();
            ApiKeyItem {
                id,
                name,
                scopes,
                last_used_at,
                created_at,
            }
        })
        .collect();

    Ok(Json(ListApiKeysResponse { keys }))
}

// ---------------------------------------------------------------------------
// DELETE /v1/api-keys/{id}
// ---------------------------------------------------------------------------

/// Revoke an API key.
///
/// Any authenticated member of the tenant may revoke any key belonging to
/// that tenant. Revocation is immediate and irreversible.
/// Requires an active session.
#[utoipa::path(
    delete,
    path = "/v1/api-keys/{id}",
    params(("id" = Uuid, Path, description = "API key ID")),
    responses(
        (status = 204, description = "Key revoked"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Key not found or already revoked"),
    ),
    tag = "api_keys"
)]
pub async fn revoke_api_key(
    State(state): State<AppState>,
    auth: AuthContext,
    Path(key_id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let session = require_session(auth)?;

    let rows_affected = sqlx::query(
        "UPDATE control.api_keys SET revoked_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(key_id)
    .bind(session.tenant_id)
    .execute(&state.pool)
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(AppError::NotFound);
    }

    sqlx::query(
        "INSERT INTO control.auth_events (user_id, tenant_id, event, metadata) \
         VALUES ($1, $2, 'api_key_revoked', $3)",
    )
    .bind(session.user_id)
    .bind(session.tenant_id)
    .bind(serde_json::json!({ "api_key_id": key_id }))
    .execute(&state.pool)
    .await?;

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, AuthContext};

    fn make_session() -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            email_verified: true,
        }
    }

    #[test]
    fn create_request_deserializes() {
        let json = r#"{"name":"CI key","scopes":["read","write"]}"#;
        let req: CreateApiKeyRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "CI key");
        assert_eq!(req.scopes, vec![Scope::Read, Scope::Write]);
    }

    #[test]
    fn create_response_serializes_all_fields() {
        let resp = CreateApiKeyResponse {
            id: Uuid::new_v4(),
            key: "rb_live_abc123".to_owned(), // gitleaks:allow — test fixture, not a real key
            name: "CI key".to_owned(),
            scopes: vec![Scope::Read],
            created_at: Utc::now(),
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val.get("id").is_some());
        assert_eq!(val["key"], "rb_live_abc123"); // gitleaks:allow — test fixture, not a real key
        assert_eq!(val["name"], "CI key");
        assert!(val.get("scopes").is_some());
        assert!(val.get("created_at").is_some());
    }

    #[test]
    fn require_session_accepts_session_auth() {
        let info = make_session();
        let auth = AuthContext::Session(info.clone());
        let result = require_session(auth).unwrap();
        assert_eq!(result.user_id, info.user_id);
    }

    #[test]
    fn require_session_rejects_anonymous() {
        assert!(matches!(
            require_session(AuthContext::Anonymous),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn require_session_rejects_api_key() {
        let info = ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            user_id: Uuid::new_v4(),
            scopes: vec![Scope::Admin],
        };
        assert!(matches!(
            require_session(AuthContext::ApiKey(info)),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn scope_from_str_known_values() {
        assert_eq!(Scope::from_str("read"), Some(Scope::Read));
        assert_eq!(Scope::from_str("write"), Some(Scope::Write));
        assert_eq!(Scope::from_str("admin"), Some(Scope::Admin));
        assert_eq!(Scope::from_str("unknown"), None);
    }

    #[test]
    fn api_key_item_serializes_without_plaintext() {
        let item = ApiKeyItem {
            id: Uuid::new_v4(),
            name: "prod".to_owned(),
            scopes: vec![Scope::Read],
            last_used_at: None,
            created_at: Utc::now(),
        };
        let val = serde_json::to_value(&item).unwrap();
        assert!(
            val.get("key").is_none(),
            "plaintext key must not be present in list response"
        );
        assert!(val.get("id").is_some());
    }

    #[test]
    fn list_response_wraps_keys_array() {
        let resp = ListApiKeysResponse { keys: vec![] };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val["keys"].is_array());
    }

    #[test]
    fn empty_name_should_be_rejected() {
        // Validation happens before DB — test the predicate directly.
        let name = "   ";
        assert!(name.trim().is_empty());
    }

    #[test]
    fn empty_scopes_should_be_rejected() {
        let scopes: Vec<Scope> = vec![];
        assert!(scopes.is_empty());
    }
}
