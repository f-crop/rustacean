//! `POST /internal/agent/sessions/{id}/events` — bulk event ingest (RUSAA-1315).
//!
//! Accepts a batch of [`RuntimeEvent`]s from agent-runner's `EventRelay`, bulk-inserts
//! them into `agents.agent_events` in a single transaction with sequential `sequence`
//! values, then fans out each inserted row to the per-session SSE bus.
//!
//! Auth: internal-only route; the `require_internal_secret` middleware is applied at
//! the router level in `routes/mod.rs`.  No user JWT is required.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use rb_schemas::{RuntimeEvent, TenantId};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{error::AppError, routes::chat::db::db_insert_chat_message, state::AppState};

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct IngestEventsRequest {
    /// Tenant that owns this session.  Verified against the DB row to prevent
    /// an attacker with the internal secret from injecting into arbitrary sessions.
    pub tenant_id: Uuid,
    /// Ordered batch of runtime events from the agent-runner relay.
    pub events: Vec<RuntimeEvent>,
}

#[derive(Debug, Serialize)]
pub struct IngestEventsResponse {
    pub inserted: usize,
}

// ---------------------------------------------------------------------------
// Event-type mapping
// ---------------------------------------------------------------------------

/// Map a [`RuntimeEvent`] variant to the `event_type` string stored in `agents.agent_events`.
fn event_type(ev: &RuntimeEvent) -> &'static str {
    match ev {
        RuntimeEvent::Text { .. } => "session.message",
        RuntimeEvent::Thinking { .. } => "session.thinking",
        RuntimeEvent::ToolUse { .. } => "session.tool_call",
        RuntimeEvent::ToolResult { .. } => "session.tool_result",
        RuntimeEvent::Error { .. } => "session.error",
        RuntimeEvent::UserInput { .. } => "session.user_input",
    }
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /internal/agent/sessions/{id}/events`
///
/// Bulk-inserts `events` into `agents.agent_events` and publishes each row to
/// the per-tenant SSE bus.  Sequence numbers are assigned atomically starting
/// from `MAX(sequence WHERE sequence >= 0) + 1` so they never collide with the
/// negative lifecycle sentinels used by `patch_session_status`.
pub async fn ingest_session_events(
    State(state): State<AppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<IngestEventsRequest>,
) -> Result<impl IntoResponse, AppError> {
    if req.events.is_empty() {
        return Ok((StatusCode::OK, Json(IngestEventsResponse { inserted: 0 })));
    }

    // SECURITY: verify the session exists and belongs to the claimed tenant.
    // Chat sessions live in control.chat_sessions (migration 021); agent sessions live in
    // agents.agent_sessions. Try the agent table first; if not found, fall back to the chat
    // table. This keeps the existing agent path unchanged while unblocking chat event relay.
    let agent_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM agents.agent_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    if let Some((session_tenant_id,)) = agent_row {
        if session_tenant_id != req.tenant_id {
            return Err(AppError::Unauthorized);
        }
        return ingest_agent_session_events(&state, session_id, &req).await;
    }

    // Chat session fallback: validate against control.chat_sessions.
    // Chat events are NOT inserted into agents.agent_events — they persist via
    // control.chat_messages through the db_insert_chat_message path. Here we only
    // fan-out to the SSE bus so the frontend receives streaming tokens.
    let chat_row: Option<(Uuid,)> =
        sqlx::query_as("SELECT tenant_id FROM control.chat_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!("DB error: {e}")))?;

    let (chat_tenant_id,) = chat_row.ok_or(AppError::NotFound)?;
    if chat_tenant_id != req.tenant_id {
        return Err(AppError::Unauthorized);
    }

    ingest_chat_session_events(&state, session_id, &req).await
}

/// Chat-session path for [`ingest_session_events`]: fans events out to the SSE bus and
/// persists each assistant turn as a single `role=assistant` row in `control.chat_messages`.
///
/// The row `body` is a JSON array of content blocks so all block types survive reload:
/// `[{"type":"text","text":"…"},{"type":"tool_use","id":"…","name":"…","input":{…}},…]`
/// Plain-`Text`-only turns also use the array format. Pre-1896 rows (plain text) continue
/// to render as plain text via the frontend fallback path in `buildTranscriptFromHistory`.
async fn ingest_chat_session_events(
    state: &AppState,
    session_id: Uuid,
    req: &IngestEventsRequest,
) -> Result<(StatusCode, Json<IngestEventsResponse>), AppError> {
    let tenant_id = TenantId::from(req.tenant_id);
    let mut fanned_out: usize = 0;

    // Content-block accumulator for the current assistant turn.
    // Consecutive `Text` payloads are merged into one block via `pending_text`.
    let mut pending_blocks: Vec<serde_json::Value> = Vec::new();
    let mut pending_text = String::new();

    for (seq, ev) in req.events.iter().enumerate() {
        let et = event_type(ev);
        let payload_value: serde_json::Value = ev
            .to_payload_json()
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);

        let seq_i64 = i64::try_from(seq + 1).unwrap_or(i64::MAX);
        let sse_data = serde_json::json!({
            "session_id": session_id,
            "event_type": et,
            "sequence": seq_i64,
            "payload": payload_value,
        });

        if let Ok(data) = serde_json::to_string(&sse_data) {
            state.sse_bus.publish_raw(&tenant_id, "session.event", data);
            fanned_out += 1;
        }

        match ev {
            RuntimeEvent::UserInput { .. } => {
                flush_pending_turn(
                    &state.pool,
                    session_id,
                    req.tenant_id,
                    &mut pending_text,
                    &mut pending_blocks,
                )
                .await;
            }
            RuntimeEvent::Text { text } => {
                pending_text.push_str(text);
            }
            RuntimeEvent::Thinking { thinking } => {
                commit_text_block(&mut pending_text, &mut pending_blocks);
                pending_blocks
                    .push(serde_json::json!({ "type": "thinking", "thinking": thinking }));
            }
            RuntimeEvent::ToolUse { id, name, input } => {
                commit_text_block(&mut pending_text, &mut pending_blocks);
                pending_blocks.push(serde_json::json!({ "type": "tool_use", "id": id, "name": name, "input": input }));
            }
            RuntimeEvent::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                commit_text_block(&mut pending_text, &mut pending_blocks);
                pending_blocks.push(serde_json::json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content, "is_error": is_error }));
            }
            RuntimeEvent::Error { .. } => {}
        }
    }

    // Flush the final assistant turn.
    flush_pending_turn(
        &state.pool,
        session_id,
        req.tenant_id,
        &mut pending_text,
        &mut pending_blocks,
    )
    .await;

    Ok((
        StatusCode::OK,
        Json(IngestEventsResponse {
            inserted: fanned_out,
        }),
    ))
}

/// Move any buffered text into `pending_blocks` as a `{"type":"text",…}` entry.
fn commit_text_block(pending_text: &mut String, pending_blocks: &mut Vec<serde_json::Value>) {
    if !pending_text.is_empty() {
        pending_blocks.push(serde_json::json!({ "type": "text", "text": pending_text.as_str() }));
        pending_text.clear();
    }
}

/// Flush accumulated content blocks as a single `role=assistant` chat message (JSON array body).
async fn flush_pending_turn(
    pool: &PgPool,
    session_id: Uuid,
    tenant_id: Uuid,
    pending_text: &mut String,
    pending_blocks: &mut Vec<serde_json::Value>,
) {
    commit_text_block(pending_text, pending_blocks);
    if pending_blocks.is_empty() {
        return;
    }
    let body = match serde_json::to_string(pending_blocks.as_slice()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(session_id = %session_id, error = %e, "chat ingest: failed to serialize content blocks");
            pending_blocks.clear();
            return;
        }
    };
    let msg_id = Uuid::new_v4();
    if let Err(e) =
        db_insert_chat_message(pool, msg_id, session_id, tenant_id, "assistant", &body).await
    {
        tracing::warn!(
            session_id = %session_id,
            error = %e,
            "chat ingest: failed to persist assistant turn — reload may miss this message"
        );
    }
    pending_blocks.clear();
}

/// Agent-session path for [`ingest_session_events`]: bulk-inserts events into
/// `agents.agent_events` and fans each row out to the per-tenant SSE bus.
async fn ingest_agent_session_events(
    state: &AppState,
    session_id: Uuid,
    req: &IngestEventsRequest,
) -> Result<(StatusCode, Json<IngestEventsResponse>), AppError> {
    let n = req.events.len();
    let mut event_types: Vec<&str> = Vec::with_capacity(n);
    let mut payloads: Vec<String> = Vec::with_capacity(n);

    for ev in &req.events {
        event_types.push(event_type(ev));
        payloads.push(
            ev.to_payload_json()
                .map_err(|e| AppError::Internal(anyhow::anyhow!("serialize error: {e}")))?,
        );
    }

    // Single-statement bulk insert. Sequences start from MAX(non-negative sequence) + 1.
    // Lifecycle sentinels use i64::MIN+1 / i64::MIN+2; stream-json events use ≥ 1.
    let rows: Vec<(i64,)> = sqlx::query_as(
        r"
        WITH base AS (
            SELECT COALESCE(MAX(sequence), 0) AS last_seq
            FROM agents.agent_events
            WHERE session_id = $1 AND sequence >= 0
        )
        INSERT INTO agents.agent_events (session_id, tenant_id, event_type, sequence, payload)
        SELECT
            $1,
            $2,
            t.event_type,
            base.last_seq + t.rn,
            t.payload::jsonb
        FROM unnest($3::text[], $4::text[])
             WITH ORDINALITY AS t(event_type, payload, rn)
        CROSS JOIN base
        RETURNING sequence
        ",
    )
    .bind(session_id)
    .bind(req.tenant_id)
    .bind(&event_types)
    .bind(&payloads)
    .fetch_all(&state.pool)
    .await
    .map_err(|e| AppError::Internal(anyhow::anyhow!("bulk insert failed: {e}")))?;

    let inserted = rows.len();

    let tenant_id = TenantId::from(req.tenant_id);
    for ((ev, et), (seq,)) in req.events.iter().zip(event_types.iter()).zip(rows.iter()) {
        let payload_value: serde_json::Value = ev
            .to_payload_json()
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);

        let sse_data = serde_json::json!({
            "session_id": session_id,
            "event_type": et,
            "sequence": seq,
            "payload": payload_value,
        });

        if let Ok(data) = serde_json::to_string(&sse_data) {
            state.sse_bus.publish_raw(&tenant_id, "session.event", data);
        }
    }

    Ok((StatusCode::OK, Json(IngestEventsResponse { inserted })))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_type_mapping_covers_all_variants() {
        assert_eq!(
            event_type(&RuntimeEvent::Text { text: "hi".into() }),
            "session.message"
        );
        assert_eq!(
            event_type(&RuntimeEvent::Thinking {
                thinking: "...".into()
            }),
            "session.thinking"
        );
        assert_eq!(
            event_type(&RuntimeEvent::ToolUse {
                id: "t".into(),
                name: "bash".into(),
                input: json!({}),
            }),
            "session.tool_call"
        );
        assert_eq!(
            event_type(&RuntimeEvent::ToolResult {
                tool_use_id: "t".into(),
                content: json!(null),
                is_error: false,
            }),
            "session.tool_result"
        );
        assert_eq!(
            event_type(&RuntimeEvent::Error {
                message: "oops".into(),
                code: None
            }),
            "session.error"
        );
        assert_eq!(
            event_type(&RuntimeEvent::UserInput {
                text: "hello".into()
            }),
            "session.user_input"
        );
    }

    #[test]
    fn ingest_request_deserializes_from_json() {
        let json_str = serde_json::to_string(&serde_json::json!({
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "events": [
                {"type": "text", "text": "Hello"},
                {"type": "error", "message": "boom"}
            ]
        }))
        .unwrap();

        let req: IngestEventsRequest = serde_json::from_str(&json_str).unwrap();
        assert_eq!(req.events.len(), 2);
        assert!(matches!(req.events[0], RuntimeEvent::Text { .. }));
        assert!(matches!(req.events[1], RuntimeEvent::Error { .. }));
    }

    #[test]
    fn empty_events_is_valid_json() {
        let json_str = serde_json::to_string(&serde_json::json!({
            "tenant_id": "00000000-0000-0000-0000-000000000001",
            "events": []
        }))
        .unwrap();
        let req: IngestEventsRequest = serde_json::from_str(&json_str).unwrap();
        assert!(req.events.is_empty());
    }

    #[test]
    fn ingest_response_serializes() {
        let resp = IngestEventsResponse { inserted: 5 };
        let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["inserted"], 5);
    }
}
