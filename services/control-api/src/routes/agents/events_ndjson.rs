//! `GET /v1/agents/sessions/{id}/log.ndjson` — full session transcript download (RUSAA-1318).
//!
//! Streams all `agents.agent_events` rows for the session as NDJSON, one JSON object per
//! line, ordered by `sequence ASC`.  Response uses chunked transfer encoding — no
//! `Content-Length` is buffered.
//!
//! Auth: normal user JWT or API key scoped to the owning tenant.  The caller must be
//! the session owner or hold the `admin` API-key scope.
//!
//! `?raw=1` bypasses redaction for `admin`-scoped callers.  In Phase 1 this is a
//! config-stub: the authorization check is enforced but actual payload redaction is
//! deferred to Phase 3 (RUSAA-1308 plan § Phase 3).

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::Response,
};
use chrono::{DateTime, Utc};
use futures::{SinkExt as _, TryStreamExt as _, channel::mpsc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

// ---------------------------------------------------------------------------
// Query parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
pub struct NdjsonParams {
    /// `?raw=1` — bypass payload redaction.  Requires `admin` API-key scope.
    /// Config-stub in Phase 1; actual redaction wired in Phase 3.
    pub raw: Option<String>,
}

impl NdjsonParams {
    fn is_raw(&self) -> bool {
        matches!(self.raw.as_deref(), Some("1"))
    }
}

// ---------------------------------------------------------------------------
// Row type
// ---------------------------------------------------------------------------

/// One row from `agents.agent_events`, serialised as a single NDJSON line.
#[derive(Debug, Serialize, sqlx::FromRow)]
struct EventRow {
    id: Uuid,
    session_id: Uuid,
    tenant_id: Uuid,
    event_type: String,
    sequence: i64,
    payload: serde_json::Value,
    created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `GET /v1/agents/sessions/{id}/log.ndjson`
///
/// Streams every row in `agents.agent_events` for the given session as
/// newline-delimited JSON ordered by `sequence ASC`.
#[utoipa::path(
    get,
    path = "/v1/agents/sessions/{id}/log.ndjson",
    params(
        ("id" = uuid::Uuid, Path, description = "Session ID"),
        ("raw" = Option<String>, Query, description = "Set to '1' to bypass redaction (requires admin scope)"),
    ),
    responses(
        (status = 200, description = "NDJSON stream of session events", content_type = "application/x-ndjson"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Insufficient permissions to access this session"),
        (status = 404, description = "Session not found"),
    ),
    tag = "agents"
)]
pub async fn session_log_ndjson(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Query(params): Query<NdjsonParams>,
    auth: AuthContext,
) -> Result<Response, AppError> {
    // Resolve caller identity.
    let caller_tenant_id = match &auth {
        AuthContext::Session(info) if info.email_verified => info.tenant_id,
        AuthContext::Session(_) => return Err(AppError::EmailNotVerified),
        AuthContext::ApiKey(info) => info.tenant_id,
        AuthContext::ExpiredSession => return Err(AppError::SessionExpired),
        AuthContext::Anonymous => return Err(AppError::Unauthorized),
    };

    // Verify the session exists and belongs to the caller's tenant.
    let row: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT tenant_id, user_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!("DB error in session_log_ndjson: {e}");
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

    // `?raw=1` requires Admin scope.  The actual redaction bypass is a Phase-3 stub.
    if params.is_raw() && !is_admin {
        return Err(AppError::InsufficientScope);
    }

    // Spawn a background task that drives the sqlx stream and forwards NDJSON chunks
    // over a futures mpsc channel.  This avoids a `'static` lifetime conflict: sqlx's
    // `.fetch()` borrows the pool by reference, which cannot be made `'static`, so we
    // move an Arc-clone of the pool into the spawned task instead.
    let pool = state.pool.clone();
    let (mut sender, receiver) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);

    tokio::spawn(async move {
        let mut row_stream = sqlx::query_as::<_, EventRow>(
            "SELECT id, session_id, tenant_id, event_type, sequence, payload, created_at \
             FROM agents.agent_events \
             WHERE session_id = $1 \
             ORDER BY sequence ASC",
        )
        .bind(session_id)
        .fetch(&pool);

        loop {
            match row_stream.try_next().await {
                Ok(Some(row)) => {
                    let Ok(mut line) = serde_json::to_string(&row) else {
                        tracing::warn!(
                            session_id = %session_id,
                            sequence = row.sequence,
                            "failed to serialize event row; skipping"
                        );
                        continue;
                    };
                    line.push('\n');
                    if sender
                        .send(Ok(axum::body::Bytes::from(line)))
                        .await
                        .is_err()
                    {
                        break; // receiver dropped — client disconnected
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::error!("DB stream error in session_log_ndjson: {e}");
                    let _ = sender.send(Err(std::io::Error::other(e.to_string()))).await;
                    break;
                }
            }
        }
    });

    let disposition = format!("attachment; filename=\"session-{session_id}.ndjson\"");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(header::CONTENT_DISPOSITION, disposition)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(receiver))
        .map_err(|e| AppError::Internal(anyhow::anyhow!("response build error: {e}")))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ndjson_params_raw_flag_parsing() {
        let with_one = NdjsonParams {
            raw: Some("1".to_owned()),
        };
        assert!(with_one.is_raw());

        let with_zero = NdjsonParams {
            raw: Some("0".to_owned()),
        };
        assert!(!with_zero.is_raw());

        let absent = NdjsonParams { raw: None };
        assert!(!absent.is_raw());

        let with_true = NdjsonParams {
            raw: Some("true".to_owned()),
        };
        assert!(!with_true.is_raw(), "only '1' is accepted");
    }

    #[test]
    fn event_row_serialises_to_ndjson_line() {
        let row = EventRow {
            id: Uuid::nil(),
            session_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            event_type: "session.message".to_owned(),
            sequence: 42,
            payload: serde_json::json!({"text": "hello"}),
            created_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        };

        let json_str = serde_json::to_string(&row).unwrap();
        // Must be valid JSON.
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["event_type"], "session.message");
        assert_eq!(parsed["sequence"], 42);
        assert_eq!(parsed["payload"]["text"], "hello");
        // Must not contain a newline (that is appended by the handler stream).
        assert!(!json_str.contains('\n'));
    }
}
