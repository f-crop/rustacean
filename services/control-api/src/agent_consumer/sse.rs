//! SSE event formatting for agent events.

use rb_schemas::AgentEvent;
use serde::Serialize;
use uuid::Uuid;

/// JSON representation of an agent event for SSE transmission.
///
/// This structure mirrors the `SessionEvent` enum in `rb-agent-runtime`
/// and provides a clean JSON shape for client consumption.
#[derive(Debug, Clone, Serialize)]
pub struct AgentEventJson {
    pub event_id: String,
    pub session_id: String,
    pub event_type: String,
    pub sequence: i64,
    pub data: serde_json::Value,
    pub occurred_at_ms: i64,
    pub trace_id: Option<String>,
}

impl AgentEventJson {
    /// Convert a protobuf AgentEvent to the JSON representation.
    ///
    /// # Errors
    ///
    /// Returns an error if the event data JSON cannot be parsed.
    pub fn from_proto(ev: &AgentEvent) -> Result<Self, serde_json::Error> {
        let data: serde_json::Value = serde_json::from_str(&ev.event_data_json)?;
        
        Ok(Self {
            event_id: ev.event_id.clone(),
            session_id: ev.session_id.clone(),
            event_type: map_event_type_string(ev.event_type),
            sequence: 0, // Will be populated by the consumer from DB
            data,
            occurred_at_ms: ev.occurred_at_ms,
            trace_id: if ev.trace_id.is_empty() { None } else { Some(ev.trace_id.clone()) },
        })
    }
}

/// Map protobuf event type to display string.
fn map_event_type_string(proto_type: i32) -> String {
    use rb_schemas::AgentEventType;
    
    match AgentEventType::try_from(proto_type) {
        Ok(AgentEventType::System) => "system",
        Ok(AgentEventType::Stdout) => "stdout",
        Ok(AgentEventType::Stderr) => "stderr",
        Ok(AgentEventType::ToolCall) => "tool_call",
        Ok(AgentEventType::ToolResult) => "tool_result",
        _ => "unknown",
    }.to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::AgentEventType;

    #[test]
    fn event_type_string_mapping() {
        assert_eq!(map_event_type_string(AgentEventType::System as i32), "system");
        assert_eq!(map_event_type_string(AgentEventType::Stdout as i32), "stdout");
        assert_eq!(map_event_type_string(AgentEventType::Stderr as i32), "stderr");
        assert_eq!(map_event_type_string(AgentEventType::ToolCall as i32), "tool_call");
        assert_eq!(map_event_type_string(AgentEventType::ToolResult as i32), "tool_result");
        assert_eq!(map_event_type_string(999), "unknown");
    }

    #[test]
    fn from_proto_parses_json_data() {
        let proto = AgentEvent {
            tenant_id: "tenant-1".to_string(),
            event_id: "evt-1".to_string(),
            session_id: "sess-1".to_string(),
            event_type: AgentEventType::System as i32,
            event_data_json: r#"{"message":"test","status":2}"#.to_string(),
            occurred_at_ms: 1700000000000,
            trace_id: "trace-1".to_string(),
            span_id: String::new(),
        };

        let json = AgentEventJson::from_proto(&proto).unwrap();
        assert_eq!(json.event_id, "evt-1");
        assert_eq!(json.session_id, "sess-1");
        assert_eq!(json.event_type, "system");
        assert_eq!(json.occurred_at_ms, 1700000000000);
        assert_eq!(json.trace_id, Some("trace-1".to_string()));
        assert_eq!(json.data["message"], "test");
    }

    #[test]
    fn from_proto_handles_empty_trace_id() {
        let proto = AgentEvent {
            tenant_id: "tenant-1".to_string(),
            event_id: "evt-1".to_string(),
            session_id: "sess-1".to_string(),
            event_type: AgentEventType::Stdout as i32,
            event_data_json: r#"{"line":"hello"}"#.to_string(),
            occurred_at_ms: 1700000000000,
            trace_id: String::new(),
            span_id: String::new(),
        };

        let json = AgentEventJson::from_proto(&proto).unwrap();
        assert_eq!(json.trace_id, None);
    }
}
