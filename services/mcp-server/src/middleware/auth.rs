use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use uuid::Uuid;

use crate::state::AppState;

const API_KEY_HEADER: &str = "X-API-Key";
const AUTHORIZATION_HEADER: &str = "Authorization";

#[derive(Debug, Clone, Default)]
pub enum AuthContext {
    #[default]
    Anonymous,
    ApiKey(ApiKeyInfo),
}

#[derive(Debug, Clone)]
pub struct ApiKeyInfo {
    #[allow(dead_code)]
    pub key_id: Uuid,
    pub tenant_id: Uuid,
    #[allow(dead_code)]
    pub user_id: Uuid,
    pub scopes: Vec<String>,
}

impl AuthContext {
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        match self {
            Self::Anonymous => &[],
            Self::ApiKey(info) => &info.scopes,
        }
    }

    #[must_use]
    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes().contains(&scope.to_owned())
    }
}

pub async fn api_key_auth_middleware(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_ctx = extract_api_key(&state, &headers).await;

    request.extensions_mut().insert(auth_ctx);

    Ok(next.run(request).await)
}

async fn extract_api_key(state: &AppState, headers: &HeaderMap) -> AuthContext {
    let key_value = headers
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .or_else(|| {
            headers
                .get(AUTHORIZATION_HEADER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
        });

    let Some(key_value) = key_value else {
        return AuthContext::Anonymous;
    };

    let result: Option<(Uuid, Uuid, Uuid, Vec<u8>)> = sqlx::query_as(
        "SELECT ak.id, ak.tenant_id, ak.user_id, ak.key_hash \
         FROM control.api_keys ak \
         JOIN control.users u ON ak.user_id = u.id \
         WHERE ak.revoked_at IS NULL \
         AND u.email_verified = true",
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();

    if let Some((key_id, tenant_id, user_id, key_hash)) = result {
        if verify_api_key(key_value, &key_hash) {
            let scopes = fetch_key_scopes(&state.pool, key_id)
                .await
                .unwrap_or_default();
            return AuthContext::ApiKey(ApiKeyInfo {
                key_id,
                tenant_id,
                user_id,
                scopes,
            });
        }
    }

    AuthContext::Anonymous
}

fn verify_api_key(provided: &str, stored_hash: &[u8]) -> bool {
    use sha2::{Digest, Sha256};

    let provided_hash = Sha256::digest(provided.as_bytes());
    provided_hash.as_slice() == stored_hash
}

async fn fetch_key_scopes(
    pool: &sqlx::PgPool,
    key_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT scope FROM control.api_key_scopes WHERE api_key_id = $1",
    )
    .bind(key_id)
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|(s,)| s).collect())
}
