//! Session event envelope (ADR-009 §5).
//!
//! Ten event types are written to `agents.agent_events` before publishing
//! to the per-tenant SSE `EventBus`.  The `SessionEvent` enum captures
//! the discriminant; each variant carries its typed payload.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCreatedPayload {
    pub runtime_kind: String,
    pub model: String,
    pub token_budget: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStartingPayload {
    pub runtime_kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRunningPayload {
    pub first_message_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPayload {
    pub tool_name: String,
    pub tool_use_id: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPayload {
    pub tool_use_id: String,
    pub content: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePayload {
    pub role: String,
    pub content: String,
    pub tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThinkingPayload {
    pub thinking: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPausedPayload {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCompletedPayload {
    pub tokens_used: i64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFailedPayload {
    pub reason: String,
    pub tokens_used: i64,
}

// ---------------------------------------------------------------------------
// SessionEvent enum — 10 types per ADR-009 §5
// ---------------------------------------------------------------------------

/// Typed session event written to `agents.agent_events` before SSE publish.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    #[serde(rename = "session.created")]
    Created(SessionCreatedPayload),
    #[serde(rename = "session.starting")]
    Starting(SessionStartingPayload),
    #[serde(rename = "session.running")]
    Running(SessionRunningPayload),
    #[serde(rename = "session.tool_call")]
    ToolCall(ToolCallPayload),
    #[serde(rename = "session.tool_result")]
    ToolResult(ToolResultPayload),
    #[serde(rename = "session.message")]
    Message(MessagePayload),
    #[serde(rename = "session.thinking")]
    Thinking(ThinkingPayload),
    #[serde(rename = "session.paused")]
    Paused(SessionPausedPayload),
    #[serde(rename = "session.completed")]
    Completed(SessionCompletedPayload),
    #[serde(rename = "session.failed")]
    Failed(SessionFailedPayload),
}

impl SessionEvent {
    /// Returns the string event-type discriminant stored in `agents.agent_events.event_type`.
    #[must_use]
    pub fn event_type(&self) -> &'static str {
        match self {
            SessionEvent::Created(_)    => "session.created",
            SessionEvent::Starting(_)   => "session.starting",
            SessionEvent::Running(_)    => "session.running",
            SessionEvent::ToolCall(_)   => "session.tool_call",
            SessionEvent::ToolResult(_) => "session.tool_result",
            SessionEvent::Message(_)    => "session.message",
            SessionEvent::Thinking(_)   => "session.thinking",
            SessionEvent::Paused(_)     => "session.paused",
            SessionEvent::Completed(_)  => "session.completed",
            SessionEvent::Failed(_)     => "session.failed",
        }
    }
}

// ---------------------------------------------------------------------------
// EventEnvelope — DB row shape for agent_events
// ---------------------------------------------------------------------------

/// Envelope written to `agents.agent_events` for each session event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub id: Uuid,
    pub session_id: Uuid,
    pub tenant_id: Uuid,
    pub event_type: String,
    pub sequence: i64,
    pub payload: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

impl EventEnvelope {
    #[must_use]
    pub fn new(session_id: Uuid, tenant_id: Uuid, sequence: i64, event: &SessionEvent) -> Self {
        Self {
            id: Uuid::new_v4(),
            session_id,
            tenant_id,
            event_type: event.event_type().to_owned(),
            sequence,
            payload: serde_json::to_value(event).unwrap_or(serde_json::json!({})),
            created_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_discriminants_are_stable() {
        let created = SessionEvent::Created(SessionCreatedPayload {
            runtime_kind: "claude_code".into(),
            model: "claude-opus-4-7".into(),
            token_budget: 100_000,
        });
        assert_eq!(created.event_type(), "session.created");
    }

    #[test]
    fn session_event_serializes_with_type_tag() {
        let ev = SessionEvent::Message(MessagePayload {
            role: "assistant".into(),
            content: "Hello".into(),
            tokens: Some(5),
        });
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "session.message");
    }

    #[test]
    fn event_envelope_carries_correct_event_type() {
        let sid = Uuid::new_v4();
        let tid = Uuid::new_v4();
        let ev = SessionEvent::Completed(SessionCompletedPayload {
            tokens_used: 42,
            duration_ms: 1234,
        });
        let env = EventEnvelope::new(sid, tid, 1, &ev);
        assert_eq!(env.event_type, "session.completed");
        assert_eq!(env.session_id, sid);
        assert_eq!(env.tenant_id, tid);
        assert_eq!(env.sequence, 1);
    }
}
