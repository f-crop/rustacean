//! Normalized event types parsed from `claude --output-format stream-json`.
//!
//! [`RuntimeEvent`] is the serde-serializable Rust-native representation used
//! on the HTTP batch path (agent-runner → control-api).  It maps 1-to-1 onto
//! the `AGENT_EVENT_KIND_*` proto enum variants added in this crate for the
//! Kafka wire format.

use serde::{Deserialize, Serialize};

use crate::AgentEventKind;

/// A normalized, serde-friendly event extracted from a `stream-json` line.
///
/// Each variant corresponds to an `AgentEventKind` proto enum value and
/// carries only the fields meaningful for that event type.  Raw Anthropic-API
/// bookkeeping fields (`usage`, `stop_sequence`, model metadata, etc.) are
/// intentionally dropped at parse time; they are not persisted or relayed.
///
/// # Wire format
///
/// Serialized with `#[serde(tag = "type", rename_all = "snake_case")]`, so the
/// JSON discriminant is the `snake_case` variant name:
///
/// ```json
/// {"type":"text","text":"Hello, world!"}
/// {"type":"thinking","thinking":"Let me reason..."}
/// {"type":"tool_use","id":"toolu_01","name":"read_file","input":{}}
/// {"type":"tool_result","tool_use_id":"toolu_01","content":null,"is_error":false}
/// {"type":"error","message":"Context limit exceeded"}
/// {"type":"user_input","text":"What is 2+2?"}
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    /// An assistant text content block (`content[].type == "text"`).
    Text { text: String },

    /// An extended thinking block (`content[].type == "thinking"`).
    Thinking { thinking: String },

    /// A tool invocation by the assistant (`content[].type == "tool_use"`).
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },

    /// The result of a tool call returned to the assistant
    /// (`content[].type == "tool_result"`).
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        is_error: bool,
    },

    /// A runtime error reported by the agent (model error, context limit, etc.).
    /// Maps to `AGENT_EVENT_KIND_ERROR` on the proto side.
    Error {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },

    /// User input relayed into the session.
    UserInput { text: String },

    /// Signals that the assistant has completed a turn (maps to `result.subtype == "success"`).
    /// This is a control signal only — it is NOT persisted as a content row.
    TurnComplete { stop_reason: String },
}

impl RuntimeEvent {
    /// Returns the corresponding `AgentEventKind` for this event.
    #[must_use]
    pub fn kind(&self) -> AgentEventKind {
        match self {
            RuntimeEvent::Text { .. } => AgentEventKind::Text,
            RuntimeEvent::Thinking { .. } => AgentEventKind::Thinking,
            RuntimeEvent::ToolUse { .. } => AgentEventKind::ToolUse,
            RuntimeEvent::ToolResult { .. } => AgentEventKind::ToolResult,
            RuntimeEvent::Error { .. } => AgentEventKind::Error,
            RuntimeEvent::UserInput { .. } => AgentEventKind::UserInput,
            RuntimeEvent::TurnComplete { .. } => AgentEventKind::TurnComplete,
        }
    }

    /// Returns the event payload as a JSON string suitable for storage in
    /// `AgentEvent::payload`.
    ///
    /// # Errors
    ///
    /// Returns `serde_json::Error` if serialization fails (in practice this
    /// only happens on non-finite floats inside `input` / `content` fields).
    pub fn to_payload_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn roundtrip(event: &RuntimeEvent) -> RuntimeEvent {
        let json = serde_json::to_string(event).expect("serialize");
        serde_json::from_str(&json).expect("deserialize")
    }

    #[test]
    fn text_roundtrip() {
        let ev = RuntimeEvent::Text {
            text: "Hello, world!".into(),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "text");
        assert_eq!(v["text"], "Hello, world!");
    }

    #[test]
    fn thinking_roundtrip() {
        let ev = RuntimeEvent::Thinking {
            thinking: "Let me reason about this...".into(),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "thinking");
        assert_eq!(v["thinking"], "Let me reason about this...");
    }

    #[test]
    fn tool_use_roundtrip() {
        let ev = RuntimeEvent::ToolUse {
            id: "toolu_01AbCd".into(),
            name: "read_file".into(),
            input: json!({"path": "src/main.rs"}),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "tool_use");
        assert_eq!(v["id"], "toolu_01AbCd");
        assert_eq!(v["name"], "read_file");
        assert_eq!(v["input"]["path"], "src/main.rs");
    }

    #[test]
    fn tool_result_roundtrip() {
        let ev = RuntimeEvent::ToolResult {
            tool_use_id: "toolu_01AbCd".into(),
            content: json!([{"type": "text", "text": "file contents"}]),
            is_error: false,
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "tool_result");
        assert_eq!(v["tool_use_id"], "toolu_01AbCd");
        assert!(!v["is_error"].as_bool().unwrap());
    }

    #[test]
    fn tool_result_error_flag_roundtrip() {
        let ev = RuntimeEvent::ToolResult {
            tool_use_id: "toolu_02XyZ".into(),
            content: json!("file not found"),
            is_error: true,
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert!(v["is_error"].as_bool().unwrap());
    }

    #[test]
    fn error_roundtrip_with_code() {
        let ev = RuntimeEvent::Error {
            message: "Context window exceeded".into(),
            code: Some("context_limit".into()),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["message"], "Context window exceeded");
        assert_eq!(v["code"], "context_limit");
    }

    #[test]
    fn error_roundtrip_without_code() {
        let ev = RuntimeEvent::Error {
            message: "Unknown error".into(),
            code: None,
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "error");
        // `code` must be absent when None (skip_serializing_if)
        assert!(v.get("code").is_none() || v["code"].is_null());
    }

    #[test]
    fn user_input_roundtrip() {
        let ev = RuntimeEvent::UserInput {
            text: "What is 2 + 2?".into(),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "user_input");
        assert_eq!(v["text"], "What is 2 + 2?");
    }

    #[test]
    fn kind_mapping_text() {
        let ev = RuntimeEvent::Text { text: "hi".into() };
        assert_eq!(ev.kind(), AgentEventKind::Text);
    }

    #[test]
    fn kind_mapping_thinking() {
        let ev = RuntimeEvent::Thinking {
            thinking: "...".into(),
        };
        assert_eq!(ev.kind(), AgentEventKind::Thinking);
    }

    #[test]
    fn kind_mapping_tool_use() {
        let ev = RuntimeEvent::ToolUse {
            id: "t".into(),
            name: "bash".into(),
            input: json!({}),
        };
        assert_eq!(ev.kind(), AgentEventKind::ToolUse);
    }

    #[test]
    fn kind_mapping_tool_result() {
        let ev = RuntimeEvent::ToolResult {
            tool_use_id: "t".into(),
            content: json!(null),
            is_error: false,
        };
        assert_eq!(ev.kind(), AgentEventKind::ToolResult);
    }

    #[test]
    fn kind_mapping_error() {
        let ev = RuntimeEvent::Error {
            message: "oops".into(),
            code: None,
        };
        assert_eq!(ev.kind(), AgentEventKind::Error);
    }

    #[test]
    fn kind_mapping_user_input() {
        let ev = RuntimeEvent::UserInput {
            text: "hello".into(),
        };
        assert_eq!(ev.kind(), AgentEventKind::UserInput);
    }

    #[test]
    fn turn_complete_roundtrip() {
        let ev = RuntimeEvent::TurnComplete {
            stop_reason: "success".into(),
        };
        assert_eq!(roundtrip(&ev), ev);
        let v: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&ev).unwrap()).unwrap();
        assert_eq!(v["type"], "turn_complete");
        assert_eq!(v["stop_reason"], "success");
    }

    #[test]
    fn kind_mapping_turn_complete() {
        let ev = RuntimeEvent::TurnComplete {
            stop_reason: "success".into(),
        };
        assert_eq!(ev.kind(), AgentEventKind::TurnComplete);
    }

    #[test]
    fn to_payload_json_produces_valid_json() {
        let ev = RuntimeEvent::ToolUse {
            id: "toolu_xyz".into(),
            name: "write_file".into(),
            input: json!({"path": "out.txt", "content": "hello"}),
        };
        let payload = ev.to_payload_json().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(parsed["type"], "tool_use");
        assert_eq!(parsed["name"], "write_file");
    }

    #[test]
    fn error_code_field_absent_when_none() {
        let ev = RuntimeEvent::Error {
            message: "fail".into(),
            code: None,
        };
        let json_str = serde_json::to_string(&ev).unwrap();
        // "code" key must not appear at all
        assert!(!json_str.contains("\"code\""));
    }
}
