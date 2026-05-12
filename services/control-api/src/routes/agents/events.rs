//! `GET /v1/agents/sessions/{id}/events` — SSE live event stream (ADR-009 §5).

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{KeepAlive, Sse},
    },
};
use futures::stream;
use rb_schemas::TenantId;
use rb_sse::{EventId, SseEnvelope};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

use super::session_lifecycle::{LIVE_STATUSES, TERMINAL_STATUSES};

#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}/events",
    params(("id" = Uuid, Path, description = "Session ID")),
    responses(
        (status = 200, description = "SSE stream"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permissions to access this session"),
        (status = 404, description = "Session not found"),
    ),
    tag = "agents"
)]
pub async fn session_events(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    auth: AuthContext,
    headers: HeaderMap,
) -> Result<axum::response::Response, AppError> {
    let caller_tenant_id = match &auth {
        AuthContext::Session(info) if info.email_verified => info.tenant_id,
        AuthContext::Session(_) => return Err(AppError::EmailNotVerified),
        AuthContext::ApiKey(info) => info.tenant_id,
        AuthContext::ExpiredSession => return Err(AppError::SessionExpired),
        AuthContext::Anonymous => return Err(AppError::Unauthorized),
    };

    let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT tenant_id, user_id, status FROM agents.agent_sessions WHERE id = $1",
    )
    .bind(session_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error in session_events: {e}");
        AppError::Internal(anyhow::anyhow!("DB query failed"))
    })?;

    let (session_tenant_id, session_owner_id, status) = row.ok_or(AppError::NotFound)?;

    if session_tenant_id != caller_tenant_id {
        return Err(AppError::InsufficientRole);
    }

    let is_owner_or_admin = match &auth {
        AuthContext::Session(info) => info.user_id == session_owner_id,
        AuthContext::ApiKey(info) => {
            info.user_id == session_owner_id || info.scopes.contains(&Scope::Admin)
        }
        _ => false,
    };

    if !is_owner_or_admin {
        return Err(AppError::InsufficientRole);
    }

    if LIVE_STATUSES.contains(&status.as_str()) {
        let tenant_id = TenantId::from(caller_tenant_id);

        let last_event_id = headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.is_empty())
            .map(|s| EventId::from(s.to_owned()));

        return Ok(state
            .sse_bus
            .subscribe_session(&tenant_id, &session_id, last_event_id.as_ref())
            .into_response());
    }

    if TERMINAL_STATUSES.contains(&status.as_str()) {
        return Ok(sse_error_response(&status));
    }

    Err(AppError::SessionNotRunning)
}

fn sse_error_response(status: &str) -> axum::response::Response {
    let error_data = serde_json::json!({
        "error": "session_not_running",
        "status": status,
        "message": format!("session is in terminal state: {status}")
    });
    let envelope = SseEnvelope::new("session.error", error_data.to_string());
    let event = envelope.to_axum_event();
    let one_shot = stream::once(async move { Ok::<_, std::convert::Infallible>(event) });
    let keepalive = KeepAlive::new().interval(std::time::Duration::from_secs(30));
    Sse::new(one_shot).keep_alive(keepalive).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    fn make_session(user_id: Uuid, tenant_id: Uuid, verified: bool) -> SessionInfo {
        SessionInfo {
            session_id: Uuid::new_v4(),
            user_id,
            tenant_id,
            email_verified: verified,
        }
    }

    fn make_api_key(user_id: Uuid, tenant_id: Uuid, scopes: Vec<Scope>) -> ApiKeyInfo {
        ApiKeyInfo {
            key_id: Uuid::new_v4(),
            tenant_id,
            user_id,
            scopes,
        }
    }

    #[test]
    fn anonymous_auth_is_unauthorized() {
        let result: Result<Uuid, AppError> = match AuthContext::Anonymous {
            AuthContext::Session(_) | AuthContext::ApiKey(_) => unreachable!(),
            AuthContext::ExpiredSession => Err(AppError::SessionExpired),
            AuthContext::Anonymous => Err(AppError::Unauthorized),
        };
        assert!(matches!(result, Err(AppError::Unauthorized)));
    }

    #[test]
    fn expired_session_returns_session_expired() {
        let result: Result<Uuid, AppError> = match AuthContext::ExpiredSession {
            AuthContext::Session(_) | AuthContext::ApiKey(_) => unreachable!(),
            AuthContext::ExpiredSession => Err(AppError::SessionExpired),
            AuthContext::Anonymous => Err(AppError::Unauthorized),
        };
        assert!(matches!(result, Err(AppError::SessionExpired)));
    }

    #[test]
    fn unverified_session_returns_email_not_verified() {
        let session = make_session(Uuid::new_v4(), Uuid::new_v4(), false);
        let result: Result<Uuid, AppError> = match AuthContext::Session(session) {
            AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
            AuthContext::Session(_) => Err(AppError::EmailNotVerified),
            _ => unreachable!(),
        };
        assert!(matches!(result, Err(AppError::EmailNotVerified)));
    }

    #[test]
    fn verified_session_returns_tenant_id() {
        let tenant_id = Uuid::new_v4();
        let session = make_session(Uuid::new_v4(), tenant_id, true);
        let result: Result<Uuid, AppError> = match AuthContext::Session(session.clone()) {
            AuthContext::Session(info) if info.email_verified => Ok(info.tenant_id),
            AuthContext::Session(_) => Err(AppError::EmailNotVerified),
            _ => unreachable!(),
        };
        assert!(matches!(result, Ok(id) if id == tenant_id));
    }

    #[test]
    fn api_key_returns_tenant_id() {
        let tenant_id = Uuid::new_v4();
        let api_key = make_api_key(Uuid::new_v4(), tenant_id, vec![Scope::Read]);
        let result: Result<Uuid, AppError> = match AuthContext::ApiKey(api_key.clone()) {
            AuthContext::ApiKey(info) => Ok(info.tenant_id),
            _ => unreachable!(),
        };
        assert!(matches!(result, Ok(id) if id == tenant_id));
    }

    #[test]
    fn session_owner_check_passes_for_same_user() {
        let user_id = Uuid::new_v4();
        let session_owner_id = user_id;
        let session = make_session(user_id, Uuid::new_v4(), true);

        let is_owner = match AuthContext::Session(session) {
            AuthContext::Session(info) => info.user_id == session_owner_id,
            _ => false,
        };
        assert!(is_owner);
    }

    #[test]
    fn session_owner_check_fails_for_different_user() {
        let user_id = Uuid::new_v4();
        let session_owner_id = Uuid::new_v4();
        let session = make_session(user_id, Uuid::new_v4(), true);

        let is_owner = match AuthContext::Session(session) {
            AuthContext::Session(info) => info.user_id == session_owner_id,
            _ => false,
        };
        assert!(!is_owner);
    }

    #[test]
    fn api_key_owner_check_passes_for_same_user() {
        let user_id = Uuid::new_v4();
        let session_owner_id = user_id;
        let api_key = make_api_key(user_id, Uuid::new_v4(), vec![Scope::Read]);

        let is_owner = match AuthContext::ApiKey(api_key) {
            AuthContext::ApiKey(info) => {
                info.user_id == session_owner_id || info.scopes.contains(&Scope::Admin)
            }
            _ => false,
        };
        assert!(is_owner);
    }

    #[test]
    fn api_key_admin_check_passes_with_admin_scope() {
        let user_id = Uuid::new_v4();
        let session_owner_id = Uuid::new_v4();
        let api_key = make_api_key(user_id, Uuid::new_v4(), vec![Scope::Admin]);

        let is_owner = match AuthContext::ApiKey(api_key) {
            AuthContext::ApiKey(info) => {
                info.user_id == session_owner_id || info.scopes.contains(&Scope::Admin)
            }
            _ => false,
        };
        assert!(is_owner);
    }

    #[test]
    fn api_key_non_owner_non_admin_fails() {
        let user_id = Uuid::new_v4();
        let session_owner_id = Uuid::new_v4();
        let api_key = make_api_key(user_id, Uuid::new_v4(), vec![Scope::Read, Scope::Write]);

        let is_owner = match AuthContext::ApiKey(api_key) {
            AuthContext::ApiKey(info) => {
                info.user_id == session_owner_id || info.scopes.contains(&Scope::Admin)
            }
            _ => false,
        };
        assert!(!is_owner);
    }

    #[tokio::test]
    async fn sse_error_response_contains_status() {
        let response = sse_error_response("terminated");
        let (parts, body) = response.into_parts();
        assert_eq!(
            parts
                .headers
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("text/event-stream")
        );

        let body_bytes = axum::body::to_bytes(body, 1024).await.unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();
        assert!(
            body_str.contains("session.error"),
            "SSE event type must be session.error, got: {body_str}"
        );
        assert!(
            body_str.contains("terminated"),
            "SSE data must contain status, got: {body_str}"
        );
    }
}
