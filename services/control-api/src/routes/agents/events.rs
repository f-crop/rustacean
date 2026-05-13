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

use std::{collections::VecDeque, convert::Infallible, sync::Arc};

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use futures::stream::{self, Stream};
use rb_schemas::TenantId;
use rb_sse::{EventId, SseEnvelope};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{AuthContext, Scope},
    state::AppState,
};

use super::session_lifecycle::{LIVE_STATUSES, TERMINAL_STATUSES};

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
// DB row type for history query
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct HistoryEventRow {
    event_type: String,
    sequence: i64,
    payload: serde_json::Value,
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
             ORDER BY sequence ASC",
        )
        .bind(session_id)
        .bind(from_seq)
        .fetch_all(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!("DB error fetching history in session_events: {e}");
            AppError::Internal(anyhow::anyhow!("DB query failed"))
        })?;

        let max_seq = history.last().map_or(from_seq - 1, |r| r.sequence);

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
// Stream builders
// ---------------------------------------------------------------------------

/// Build a merged stream:
///   Phase 1 — emit DB history rows in sequence order.
///   Phase 2 — emit live broadcast events for this session, skipping any whose
///              sequence is already covered by history (deduplication for the
///              race window between DB query and bus subscription).
fn history_join_stream(
    session_id: Uuid,
    history: Vec<HistoryEventRow>,
    max_history_seq: i64,
    live_rx: tokio::sync::broadcast::Receiver<Arc<SseEnvelope>>,
) -> impl Stream<Item = Result<Event, Infallible>> + Send + 'static {
    struct State {
        history: VecDeque<HistoryEventRow>,
        session_id: Uuid,
        max_history_seq: i64,
        rx: tokio::sync::broadcast::Receiver<Arc<SseEnvelope>>,
    }

    stream::unfold(
        State {
            history: VecDeque::from(history),
            session_id,
            max_history_seq,
            rx: live_rx,
        },
        |mut s| async move {
            use tokio::sync::broadcast::error::RecvError;

            // Phase 1 — drain history queue
            if let Some(row) = s.history.pop_front() {
                let ev = history_row_to_event(s.session_id, &row);
                return Some((Ok(ev), s));
            }

            // Phase 2 — live broadcast
            loop {
                match s.rx.recv().await {
                    Ok(env) => {
                        if !data_matches_session(&env.data, s.session_id) {
                            continue;
                        }
                        // Deduplicate events already covered by history.
                        if let Some(seq) = extract_sequence(&env.data) {
                            if seq <= s.max_history_seq {
                                continue;
                            }
                        }
                        return Some((Ok(env.to_axum_event()), s));
                    }

                    Err(RecvError::Lagged(n)) => {
                        metrics::counter!("rb_sse_dropped_total", "reason" => "lagged")
                            .increment(n);
                        let reset = SseEnvelope::stream_reset().to_axum_event();
                        return Some((Ok(reset), s));
                    }

                    Err(RecvError::Closed) => return None,
                }
            }
        },
    )
}

/// Build a finite one-shot SSE response for a terminal session:
/// emit history rows then a synthetic `session.completed` event and close.
fn terminal_history_stream(
    session_id: Uuid,
    history: &[HistoryEventRow],
    status: &str,
) -> axum::response::Response {
    let mut items: Vec<Result<Event, Infallible>> = history
        .iter()
        .map(|row| Ok(history_row_to_event(session_id, row)))
        .collect();

    let synthetic_data = serde_json::json!({
        "session_id": session_id,
        "status": status,
    })
    .to_string();
    items.push(Ok(Event::default()
        .event("session.completed")
        .data(synthetic_data)));

    let keepalive = KeepAlive::new().interval(std::time::Duration::from_secs(30));
    Sse::new(stream::iter(items))
        .keep_alive(keepalive)
        .into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a DB history row into the same SSE wire format that the ingest
/// handler publishes via `publish_raw`: event name `session.event`, data JSON
/// `{"session_id","event_type","sequence","payload"}`.
fn history_row_to_event(session_id: Uuid, row: &HistoryEventRow) -> Event {
    let data = serde_json::json!({
        "session_id": session_id,
        "event_type": row.event_type,
        "sequence": row.sequence,
        "payload": row.payload,
    })
    .to_string();
    Event::default().event("session.event").data(data)
}

/// Return `true` if the SSE data JSON contains `"session_id": "<session_id>"`.
fn data_matches_session(data: &str, session_id: Uuid) -> bool {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v.get("session_id")?.as_str().map(String::from))
        .is_some_and(|s| s == session_id.to_string())
}

/// Extract the `"sequence"` field (i64) from an SSE data JSON string.
fn extract_sequence(data: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v.get("sequence")?.as_i64())
}

/// One-shot terminal error SSE frame (pre-history-join behaviour for terminal sessions).
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use futures::StreamExt as _;
    use rb_sse::{EventBus, SseConfig, SseEnvelope, TenantId};
    use uuid::Uuid;

    use super::*;
    use crate::middleware::auth::{ApiKeyInfo, SessionInfo};

    // ── Auth guard unit tests (identical to the original file) ────────────

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

    // ── Helper unit tests ─────────────────────────────────────────────────

    #[test]
    fn extract_sequence_parses_correctly() {
        let data =
            r#"{"session_id":"x","sequence":42,"event_type":"session.message","payload":{}}"#;
        assert_eq!(extract_sequence(data), Some(42));
    }

    #[test]
    fn extract_sequence_returns_none_for_missing_field() {
        assert_eq!(
            extract_sequence(r#"{"event_type":"session.message"}"#),
            None
        );
    }

    #[test]
    fn extract_sequence_returns_none_for_invalid_json() {
        assert_eq!(extract_sequence("not-json"), None);
    }

    #[test]
    fn data_matches_session_true_for_matching_id() {
        let sid = Uuid::new_v4();
        let data = serde_json::json!({"session_id": sid}).to_string();
        assert!(data_matches_session(&data, sid));
    }

    #[test]
    fn data_matches_session_false_for_different_id() {
        let sid = Uuid::new_v4();
        let other = Uuid::new_v4();
        let data = serde_json::json!({"session_id": other}).to_string();
        assert!(!data_matches_session(&data, sid));
    }

    #[test]
    fn data_matches_session_false_for_missing_field() {
        let sid = Uuid::new_v4();
        assert!(!data_matches_session(
            r#"{"event_type":"session.message"}"#,
            sid
        ));
    }

    #[test]
    fn history_row_to_event_produces_session_event_name() {
        let session_id = Uuid::new_v4();
        let row = HistoryEventRow {
            event_type: "session.message".to_owned(),
            sequence: 7,
            payload: serde_json::json!({"text": "hi"}),
        };
        let ev = history_row_to_event(session_id, &row);
        // We can't inspect axum's Event fields directly, but we can verify the
        // SSE wire bytes contain the expected values.
        let raw = format!("{ev:?}");
        // axum's Event Debug representation includes the field values.
        assert!(raw.contains("session.event") || raw.contains("session_id"));
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

    // ── terminal_history_stream unit test ────────────────────────────────

    #[tokio::test]
    async fn terminal_history_stream_emits_history_then_completion() {
        let session_id = Uuid::new_v4();
        let history = vec![
            HistoryEventRow {
                event_type: "session.message".to_owned(),
                sequence: 1,
                payload: serde_json::json!({}),
            },
            HistoryEventRow {
                event_type: "session.message".to_owned(),
                sequence: 2,
                payload: serde_json::json!({}),
            },
        ];

        let response = terminal_history_stream(session_id, &history, "terminated");
        let (parts, body) = response.into_parts();
        assert_eq!(
            parts
                .headers
                .get("content-type")
                .map(|v| v.to_str().unwrap()),
            Some("text/event-stream")
        );

        let body_bytes = axum::body::to_bytes(body, 4096).await.unwrap();
        let body_str = String::from_utf8(body_bytes.to_vec()).unwrap();

        // Must include 2 history events + 1 completion event.
        assert!(
            body_str.contains("session.event"),
            "must contain history event frames: {body_str}"
        );
        assert!(
            body_str.contains("session.completed"),
            "must contain synthetic completion frame: {body_str}"
        );
        assert!(
            body_str.contains("terminated"),
            "completion frame must include status: {body_str}"
        );
    }

    // ── history_join_stream integration test ─────────────────────────────
    //
    // Simulates 50 historical events (sequences 1–50) + 10 live events
    // (sequences 51–60).  Race-window events with sequences 46–50 are
    // pre-published to the channel (simulating events that arrive during the
    // DB history query); they must be silently deduplicated because their
    // sequence numbers are ≤ max_history_seq (50).  The stream must emit
    // exactly 60 events total with no gaps and no duplicates.

    fn make_live_envelope(session_id: Uuid, seq: i64) -> Arc<SseEnvelope> {
        let data = serde_json::json!({
            "session_id": session_id,
            "event_type": "session.message",
            "sequence": seq,
            "payload": {},
        })
        .to_string();
        Arc::new(SseEnvelope::new("session.event", data))
    }

    #[tokio::test]
    async fn history_join_stream_50_historical_plus_10_live_no_gaps_no_duplicates() {
        let session_id = Uuid::new_v4();

        // Build 50 history rows (sequences 1–50).
        let history: Vec<HistoryEventRow> = (1i64..=50)
            .map(|seq| HistoryEventRow {
                event_type: "session.message".to_owned(),
                sequence: seq,
                payload: serde_json::json!({}),
            })
            .collect();

        // Capacity must hold all pre-published events before the stream drains them.
        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<SseEnvelope>>(128);

        // Race-window: publish sequences 46–50 before the stream starts.
        // All have seq <= max_history_seq (50) so they will be deduplicated.
        for seq in 46i64..=50 {
            tx.send(make_live_envelope(session_id, seq)).unwrap();
        }

        // Live events: publish sequences 51–60 (10 new events).
        for seq in 51i64..=60 {
            tx.send(make_live_envelope(session_id, seq)).unwrap();
        }

        // Drop the sender so the stream terminates after exhausting the channel.
        drop(tx);

        // Collect all events from the stream.
        let stream = std::pin::pin!(history_join_stream(session_id, history, 50, rx));
        let items: Vec<_> = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            stream.collect::<Vec<_>>(),
        )
        .await
        .expect("stream did not complete within 2 s");

        // Must see exactly 60 events (50 history + 10 live; 5 race duplicates dropped).
        assert_eq!(items.len(), 60, "expected 60 events, got {}", items.len());
        for item in &items {
            assert!(item.is_ok(), "stream item must be Ok");
        }
    }

    // A second channel isolates one session from another tenant's events.
    #[tokio::test]
    async fn history_join_stream_ignores_other_session_events() {
        let my_session = Uuid::new_v4();
        let other_session = Uuid::new_v4();

        let (tx, rx) = tokio::sync::broadcast::channel::<Arc<SseEnvelope>>(32);

        // Publish 3 events for the other session and 1 for my session.
        for _ in 0..3 {
            tx.send(make_live_envelope(other_session, 99)).unwrap();
        }
        tx.send(make_live_envelope(my_session, 1)).unwrap();

        let mut stream = std::pin::pin!(history_join_stream(my_session, vec![], -1, rx));

        let item = tokio::time::timeout(std::time::Duration::from_millis(300), stream.next())
            .await
            .expect("timeout: my_session event should arrive")
            .expect("stream ended");

        assert!(item.is_ok());
    }

    // Verify that the EventBus.subscribe_raw_for_tenant API is correct.
    #[tokio::test]
    async fn subscribe_raw_for_tenant_receives_published_events() {
        let bus = EventBus::new(SseConfig::default());
        let tenant = TenantId::new();
        let session_id = Uuid::new_v4();

        let mut rx = bus.subscribe_raw_for_tenant(&tenant);

        let data = serde_json::json!({
            "session_id": session_id,
            "event_type": "session.message",
            "sequence": 1,
            "payload": {},
        })
        .to_string();
        bus.publish_raw(&tenant, "session.event", data.clone());

        let env = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timeout")
            .expect("channel closed");

        assert_eq!(env.data, data);
    }
}
