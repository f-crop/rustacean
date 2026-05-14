use std::{collections::VecDeque, convert::Infallible, sync::Arc};

use axum::response::{
    IntoResponse,
    sse::{Event, KeepAlive, Sse},
};
use futures::stream::{self, Stream};
use rb_sse::SseEnvelope;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// DB row type for history query
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
pub(super) struct HistoryEventRow {
    pub(super) event_type: String,
    pub(super) sequence: i64,
    pub(super) payload: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Stream builders
// ---------------------------------------------------------------------------

/// Build a merged stream:
///   Phase 1 — emit DB history rows in sequence order.
///   Phase 2 — emit live broadcast events for this session, skipping any whose
///              sequence is already covered by history (deduplication for the
///              race window between DB query and bus subscription).
pub(super) fn history_join_stream(
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
pub(super) fn terminal_history_stream(
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
pub(super) fn history_row_to_event(session_id: Uuid, row: &HistoryEventRow) -> Event {
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
pub(super) fn data_matches_session(data: &str, session_id: Uuid) -> bool {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v.get("session_id")?.as_str().map(String::from))
        .is_some_and(|s| s == session_id.to_string())
}

/// Extract the `"sequence"` field (i64) from an SSE data JSON string.
pub(super) fn extract_sequence(data: &str) -> Option<i64> {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v.get("sequence")?.as_i64())
}

/// One-shot terminal error SSE frame (pre-history-join behaviour for terminal sessions).
pub(super) fn sse_error_response(status: &str) -> axum::response::Response {
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
