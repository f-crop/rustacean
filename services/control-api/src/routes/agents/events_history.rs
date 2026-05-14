//! `GET /v1/agents/sessions/{id}/events/history` — paged event history (RUSAA-1317).
//!
//! Returns a JSON page of `agents.agent_events` rows ordered by `sequence ASC`.
//!
//! Query parameters:
//!   - `after`: exclusive lower bound on `sequence` (omit to start from the beginning)
//!   - `limit`: number of events per page (default 100, max 500)
//!   - `raw`: set to `"1"` to bypass payload redaction (requires `admin` API-key scope)
//!
//! Response envelope: `{"events": [...], "next_seq": <n | null>}`
//!   `next_seq` is the `sequence` of the last event in this page when more pages exist,
//!   `null` when this is the final page.
//!
//! Auth: normal user JWT or API key scoped to the owning tenant.
//! The caller must be the session owner or hold the `admin` API-key scope.
//!
//! By default all event payloads are redacted via `rb_observability::redact_value`
//! before returning.  Admin-scoped callers can use `?raw=1` to receive unredacted
//! payloads.

use axum::{
    Json,
    extract::{Path, Query, State},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 500;

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct HistoryParams {
    /// Exclusive lower bound on `sequence`.  Omit to start from the beginning.
    pub after: Option<i64>,
    /// Page size.  Default 100, max 500.
    pub limit: Option<i64>,
    /// `?raw=1` — bypass payload redaction.  Requires `admin` API-key scope.
    pub raw: Option<String>,
}

impl HistoryParams {
    fn is_raw(&self) -> bool {
        matches!(self.raw.as_deref(), Some("1"))
    }
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, sqlx::FromRow, utoipa::ToSchema)]
pub struct EventItem {
    pub id: Uuid,
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub event_type: String,
    pub sequence: i64,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct HistoryResponse {
    pub events: Vec<EventItem>,
    pub next_seq: Option<i64>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `GET /v1/agents/sessions/{id}/events/history`
///
/// Returns a paged slice of `agents.agent_events` for the given session.
/// Uses a `limit + 1` probe to determine whether a next page exists without
/// a separate COUNT query.
#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}/events/history",
    params(
        ("id" = Uuid, Path, description = "Session ID"),
        ("after" = Option<i64>, Query, description = "Exclusive lower bound on sequence (omit for beginning)"),
        ("limit" = Option<i64>, Query, description = "Page size (default 100, max 500)"),
        ("raw" = Option<String>, Query, description = "Set to '1' to bypass redaction (requires admin scope; default: redacted)"),
    ),
    responses(
        (status = 200, description = "Page of events"),
        (status = 400, description = "Invalid limit parameter"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permissions"),
        (status = 404, description = "Session not found"),
    ),
    tag = "agents"
)]
pub async fn session_events_history(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(params): Query<HistoryParams>,
    auth: AuthContext,
) -> Result<impl IntoResponse, AppError> {
    // Resolve caller identity.
    let caller_tenant_id = match &auth {
        AuthContext::Session(info) if info.email_verified => info.tenant_id,
        AuthContext::Session(_) => return Err(AppError::EmailNotVerified),
        AuthContext::ApiKey(info) => info.tenant_id,
        AuthContext::ExpiredSession => return Err(AppError::SessionExpired),
        AuthContext::Anonymous => return Err(AppError::Unauthorized),
    };

    // Validate limit.
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::InvalidInput);
    }
    // Safety: limit is validated to be in 1..=500, which fits usize on all targets.
    let limit_usize = usize::try_from(limit).expect("limit validated above");

    // Verify the session exists and belongs to the caller's tenant.
    let row: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT tenant_id, user_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error in session_events_history: {e}");
                AppError::Internal(anyhow::anyhow!("DB query failed"))
            })?;

    let (session_tenant_id, session_owner_id) = row.ok_or(AppError::NotFound)?;

    if session_tenant_id != caller_tenant_id {
        return Err(AppError::InsufficientRole);
    }

    let is_admin = match &auth {
        AuthContext::ApiKey(info) => info.scopes.contains(&Scope::Admin),
        _ => false,
    };

    let is_owner = match &auth {
        AuthContext::Session(info) => info.user_id == session_owner_id,
        AuthContext::ApiKey(info) => info.user_id == session_owner_id,
        _ => false,
    };

    if !is_owner && !is_admin {
        return Err(AppError::InsufficientRole);
    }

    // `?raw=1` requires Admin scope.
    if params.is_raw() && !is_admin {
        return Err(AppError::InsufficientScope);
    }

    let should_redact = !params.is_raw();

    // `after` is an exclusive lower bound; default -1 so `sequence > -1` covers all rows.
    let after_seq = params.after.unwrap_or(-1);

    // Fetch `limit + 1` rows to detect whether a next page exists.
    let mut rows: Vec<EventItem> = sqlx::query_as(
        "SELECT id, session_id, tenant_id, event_type, sequence, payload, created_at \
         FROM agents.agent_events \
         WHERE session_id = $1 AND sequence > $2 \
         ORDER BY sequence ASC \
         LIMIT $3",
    )
    .bind(session_id)
    .bind(after_seq)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| {
        tracing::error!("DB error fetching history page: {e}");
        AppError::Internal(anyhow::anyhow!("DB query failed"))
    })?;

    // If we got more rows than the requested limit, there is a next page.
    let next_seq = if rows.len() > limit_usize {
        rows.truncate(limit_usize);
        rows.last().map(|r| r.sequence)
    } else {
        None
    };

    // Redact payloads unless the caller holds admin scope and requested raw output.
    if should_redact {
        for ev in &mut rows {
            ev.payload = rb_observability::redact_value(&ev.payload);
        }
    }

    Ok(Json(HistoryResponse {
        events: rows,
        next_seq,
    }))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_limit_is_100() {
        assert_eq!(DEFAULT_LIMIT, 100);
    }

    #[test]
    fn max_limit_is_500() {
        assert_eq!(MAX_LIMIT, 500);
    }

    #[test]
    fn history_response_next_seq_null_when_no_more() {
        let resp = HistoryResponse {
            events: vec![],
            next_seq: None,
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert!(val["next_seq"].is_null());
        assert!(val["events"].is_array());
    }

    #[test]
    fn history_response_next_seq_present_when_more_pages() {
        let resp = HistoryResponse {
            events: vec![],
            next_seq: Some(42),
        };
        let val = serde_json::to_value(&resp).unwrap();
        assert_eq!(val["next_seq"], 42);
    }

    #[test]
    fn history_params_after_defaults_to_none() {
        let p: HistoryParams = serde_json::from_str("{}").unwrap();
        assert!(p.after.is_none());
        assert!(p.limit.is_none());
    }

    #[test]
    fn history_params_parses_after_and_limit() {
        let p: HistoryParams = serde_json::from_str(r#"{"after":10,"limit":50}"#).unwrap();
        assert_eq!(p.after, Some(10));
        assert_eq!(p.limit, Some(50));
    }

    #[test]
    fn event_item_serializes_all_fields() {
        let item = EventItem {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            event_type: "session.message".to_owned(),
            sequence: 7,
            payload: serde_json::json!({"text": "hello"}),
            created_at: chrono::Utc::now(),
        };
        let val = serde_json::to_value(&item).unwrap();
        assert!(val.get("id").is_some());
        assert_eq!(val["event_type"], "session.message");
        assert_eq!(val["sequence"], 7);
        assert!(val["payload"].is_object());
        assert!(val["created_at"].is_string());
    }
}
