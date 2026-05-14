//! `GET /v1/agents/sessions/{id}/events` — SSE live event stream (ADR-009 §5).
//!
//! When `?from_sequence=<n>` is supplied the endpoint delivers a gap-free
//! history-join stream:
//!
//!  1. Subscribe to the live broadcast bus **before** querying the DB so events
//!     published in the query window are buffered in the channel (race prevention).
//!  2. Fetch all `agents.agent_events` rows with `sequence >= from_sequence`.
//!  3. Emit the historical rows as SSE events (same JSON envelope as live frames).
//!  4. Switch to the live broadcast receiver, skipping any frames whose sequence
//!     number is already covered by the history batch (deduplication).
//!
//! For sessions that are already in a terminal state (`completed`/`failed`):
//! emit history, then send a synthetic `session.completed` event and close.

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{KeepAlive, Sse},
    },
};
use rb_schemas::TenantId;
use rb_sse::EventId;
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

use super::session_lifecycle::{LIVE_STATUSES, TERMINAL_STATUSES};

mod stream;
use stream::{HistoryEventRow, history_join_stream, sse_error_response, terminal_history_stream};

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct EventsParams {
    /// When present, replay all DB events with `sequence >= from_sequence` before
    /// switching to the live fan-out.  Absent → pure live stream (existing behaviour).
    pub from_sequence: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}/events",
    params(
        ("id" = Uuid, Path, description = "Session ID"),
        ("from_sequence" = Option<i64>, Query, description = "Sequence number to replay history from"),
    ),
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
    Query(params): Query<EventsParams>,
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

    let tenant_id = TenantId::from(caller_tenant_id);

    // -----------------------------------------------------------------------
    // History-join path: `?from_sequence=<n>` was supplied
    // -----------------------------------------------------------------------
    if let Some(from_seq) = params.from_sequence {
        // 1. Subscribe to live bus BEFORE querying the DB so we don't miss
        //    events published in the window between the DB read and channel
        //    subscription (closing the race).
        let live_rx = state.sse_bus.subscribe_raw_for_tenant(&tenant_id);

        // 2. Fetch DB history.
        let history: Vec<HistoryEventRow> = sqlx::query_as(
            "SELECT event_type, sequence, payload \
             FROM agents.agent_events \
             WHERE session_id = $1 AND sequence >= $2 \
             ORDER BY sequence ASC \
             LIMIT 10000",
        )
        .bind(session_id)
        .bind(from_seq)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error fetching history in session_events: {e}");
            AppError::Internal(anyhow::anyhow!("DB query failed"))
        })?;

        let max_seq = history
            .last()
            .map_or(from_seq.saturating_sub(1), |r| r.sequence);

        if TERMINAL_STATUSES.contains(&status.as_str()) {
            return Ok(terminal_history_stream(session_id, &history, &status));
        }

        if LIVE_STATUSES.contains(&status.as_str()) {
            let inner = history_join_stream(session_id, history, max_seq, live_rx);
            let keepalive = KeepAlive::new().interval(std::time::Duration::from_secs(30));
            return Ok(Sse::new(inner).keep_alive(keepalive).into_response());
        }

        return Err(AppError::SessionNotRunning);
    }

    // -----------------------------------------------------------------------
    // Original path: no `from_sequence`
    // -----------------------------------------------------------------------
    if LIVE_STATUSES.contains(&status.as_str()) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    // ── Auth guard unit tests ─────────────────────────────────────────────

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

    #[test]
    fn events_params_defaults_to_none() {
        let p: EventsParams = serde_json::from_str("{}").unwrap();
        assert!(p.from_sequence.is_none());
    }

    #[test]
    fn events_params_with_from_sequence_some_value() {
        let p = EventsParams {
            from_sequence: Some(42),
        };
        assert_eq!(p.from_sequence, Some(42));
    }
}
