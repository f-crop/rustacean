//! Parses `claude --output-format stream-json` stdout lines into typed
//! [`RuntimeEvent`] values from `rb-schemas`.
//!
//! Each NDJSON line from the Claude Code CLI may expand into zero or more
//! `RuntimeEvent` values (e.g., an assistant message with thinking + text +
//! `tool_use` yields three events).  Unknown or malformed lines are logged at
//! debug level and produce an empty event list — this module never panics.

use rb_schemas::RuntimeEvent;
use serde::Deserialize;

/// Parses raw NDJSON lines emitted by `claude --output-format stream-json`
/// into typed [`RuntimeEvent`] values.
pub struct StreamJsonNormalizer;

impl StreamJsonNormalizer {
    /// Parse one NDJSON line from the Claude CLI into zero or more events.
    ///
    /// Returns an empty `Vec` for:
    /// - Lifecycle/system lines (`system` init, successful `result`)
    /// - Unrecognized top-level `type` values
    /// - Empty or whitespace-only lines
    /// - Lines that fail JSON parsing
    pub fn normalize_line(line: &str) -> Vec<RuntimeEvent> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return vec![];
        }

        let stream_line: StreamLine = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    line_preview = %&trimmed[..trimmed.len().min(120)],
                    "stream-json parse error — skipping line"
                );
                return vec![];
            }
        };

        match stream_line {
            StreamLine::Assistant(p) => extract_assistant_events(p),
            StreamLine::User(p) => extract_user_events(p),
            StreamLine::Result(p) => extract_result_events(p),
            StreamLine::System => vec![],
            StreamLine::Unknown => {
                tracing::debug!(
                    line_preview = %&trimmed[..trimmed.len().min(120)],
                    "unrecognized stream-json top-level type — skipping"
                );
                vec![]
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Wire-format deserialization structs (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamLine {
    System,
    Assistant(AssistantPayload),
    User(UserPayload),
    Result(ResultPayload),
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct AssistantPayload {
    message: AssistantMessage,
}

#[derive(Deserialize)]
struct AssistantMessage {
    #[serde(default)]
    content: Vec<AssistantContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AssistantContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct UserPayload {
    message: UserMessage,
}

#[derive(Deserialize)]
struct UserMessage {
    #[serde(default)]
    content: Vec<UserContentBlock>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum UserContentBlock {
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        content: serde_json::Value,
        #[serde(default)]
        is_error: bool,
    },
    Text {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Deserialize)]
struct ResultPayload {
    subtype: String,
    #[serde(default)]
    is_error: bool,
    error: Option<String>,
    result: Option<String>,
}

// ---------------------------------------------------------------------------
// Extraction helpers
// ---------------------------------------------------------------------------

fn extract_assistant_events(payload: AssistantPayload) -> Vec<RuntimeEvent> {
    payload
        .message
        .content
        .into_iter()
        .filter_map(|block| match block {
            AssistantContentBlock::Text { text } => Some(RuntimeEvent::Text { text }),
            AssistantContentBlock::Thinking { thinking } => {
                Some(RuntimeEvent::Thinking { thinking })
            }
            AssistantContentBlock::ToolUse { id, name, input } => {
                Some(RuntimeEvent::ToolUse { id, name, input })
            }
            AssistantContentBlock::Unknown => {
                tracing::debug!("unknown assistant content block type — skipping");
                None
            }
        })
        .collect()
}

fn extract_user_events(payload: UserPayload) -> Vec<RuntimeEvent> {
    payload
        .message
        .content
        .into_iter()
        .filter_map(|block| match block {
            UserContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some(RuntimeEvent::ToolResult {
                tool_use_id,
                content,
                is_error,
            }),
            UserContentBlock::Text { text } if !text.is_empty() => {
                Some(RuntimeEvent::UserInput { text })
            }
            UserContentBlock::Text { .. } => None,
            UserContentBlock::Unknown => {
                tracing::debug!("unknown user content block type — skipping");
                None
            }
        })
        .collect()
}

fn extract_result_events(payload: ResultPayload) -> Vec<RuntimeEvent> {
    if payload.is_error || payload.subtype.contains("error") {
        let message = payload
            .error
            .or(payload.result)
            .unwrap_or_else(|| "Unknown error".to_string());
        vec![RuntimeEvent::Error {
            message,
            code: Some(payload.subtype),
        }]
    } else {
        vec![RuntimeEvent::TurnComplete {
            stop_reason: payload.subtype,
        }]
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normalize(line: &str) -> Vec<RuntimeEvent> {
        StreamJsonNormalizer::normalize_line(line)
    }

    // --- system lines ---

    #[test]
    fn system_init_produces_no_events() {
        let line = r#"{"type":"system","subtype":"init","cwd":"/workspace","session_id":"s1","tools":[],"mcp_servers":[],"model":"claude-3-7-sonnet-20250219","permissionMode":"default","apiKeySource":"ANTHROPIC_API_KEY"}"#;
        assert!(normalize(line).is_empty());
    }

    // --- assistant text ---

    #[test]
    fn assistant_text_block_yields_text_event() {
        let line = json!({
            "type": "assistant",
            "message": {
                "id": "msg_01",
                "type": "message",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "content": [{"type": "text", "text": "Hello, world!"}],
                "stop_reason": "end_turn",
                "usage": {"input_tokens": 10, "output_tokens": 5}
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::Text {
                text: "Hello, world!".into()
            }
        );
    }

    // --- thinking ---

    #[test]
    fn assistant_thinking_block_yields_thinking_event() {
        let line = json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "thinking", "thinking": "Let me reason..."}],
                "stop_reason": "end_turn"
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::Thinking {
                thinking: "Let me reason...".into()
            }
        );
    }

    // --- tool_use ---

    #[test]
    fn assistant_tool_use_block_yields_tool_use_event() {
        let line = json!({
            "type": "assistant",
            "message": {
                "content": [{
                    "type": "tool_use",
                    "id": "toolu_01AbCd",
                    "name": "bash",
                    "input": {"command": "ls -la"}
                }],
                "stop_reason": "tool_use"
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::ToolUse {
                id: "toolu_01AbCd".into(),
                name: "bash".into(),
                input: json!({"command": "ls -la"})
            }
        );
    }

    // --- multiple content blocks ---

    #[test]
    fn assistant_message_with_multiple_blocks_yields_multiple_events() {
        let line = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "thinking", "thinking": "I'll use bash"},
                    {"type": "text", "text": "Let me check the files."},
                    {"type": "tool_use", "id": "toolu_02", "name": "read_file", "input": {"path": "src/main.rs"}}
                ],
                "stop_reason": "tool_use"
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], RuntimeEvent::Thinking { .. }));
        assert!(matches!(events[1], RuntimeEvent::Text { .. }));
        assert!(matches!(events[2], RuntimeEvent::ToolUse { .. }));
    }

    // --- tool_result ---

    #[test]
    fn user_tool_result_block_yields_tool_result_event() {
        let line = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01AbCd",
                    "content": [{"type": "text", "text": "file.txt\n"}],
                    "is_error": false
                }]
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::ToolResult {
                tool_use_id: "toolu_01AbCd".into(),
                content: json!([{"type": "text", "text": "file.txt\n"}]),
                is_error: false
            }
        );
    }

    #[test]
    fn user_tool_result_error_flag_propagated() {
        let line = json!({
            "type": "user",
            "message": {
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_XY",
                    "content": "file not found",
                    "is_error": true
                }]
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            RuntimeEvent::ToolResult { is_error: true, .. }
        ));
    }

    // --- user input (text block in user message) ---

    #[test]
    fn user_text_block_yields_user_input_event() {
        let line = json!({
            "type": "user",
            "message": {
                "content": [{"type": "text", "text": "What is 2 + 2?"}]
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::UserInput {
                text: "What is 2 + 2?".into()
            }
        );
    }

    // --- result: success ---

    #[test]
    fn result_success_produces_turn_complete_event() {
        let line = json!({
            "type": "result",
            "subtype": "success",
            "cost_usd": 0.01,
            "is_error": false,
            "num_turns": 1,
            "result": "42",
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::TurnComplete {
                stop_reason: "success".into()
            }
        );
    }

    // --- result: error ---

    #[test]
    fn result_error_yields_error_event() {
        let line = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "error": "Context window exceeded",
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0],
            RuntimeEvent::Error {
                message: "Context window exceeded".into(),
                code: Some("error_during_execution".into())
            }
        );
    }

    // --- unknown top-level type ---

    #[test]
    fn unknown_top_level_type_produces_no_events() {
        let line = r#"{"type":"debug","data":"some internal event"}"#;
        assert!(normalize(line).is_empty());
    }

    // --- unknown content block ---

    #[test]
    fn unknown_assistant_content_block_skipped_gracefully() {
        let line = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Before unknown"},
                    {"type": "redacted_thinking", "data": "encrypted_blob"},
                    {"type": "text", "text": "After unknown"}
                ],
                "stop_reason": "end_turn"
            },
            "session_id": "s1"
        })
        .to_string();

        let events = normalize(&line);
        // Only the two text events; redacted_thinking is skipped
        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], RuntimeEvent::Text { text } if text == "Before unknown"));
        assert!(matches!(&events[1], RuntimeEvent::Text { text } if text == "After unknown"));
    }

    // --- malformed / non-JSON input ---

    #[test]
    fn malformed_json_produces_no_events_no_panic() {
        assert!(normalize("not json at all").is_empty());
        assert!(normalize("{broken json}").is_empty());
        assert!(normalize("").is_empty());
        assert!(normalize("   ").is_empty());
    }

    // --- sequence ordering invariant ---

    #[test]
    fn result_error_subtype_prefix_match() {
        // Any subtype containing "error" should produce an Error event
        let subtypes = [
            "error_during_execution",
            "error_max_turns",
            "error_context_limit",
        ];
        for subtype in subtypes {
            let line = json!({
                "type": "result",
                "subtype": subtype,
                "is_error": true,
                "error": "something went wrong",
                "session_id": "s1"
            })
            .to_string();
            let events = normalize(&line);
            assert_eq!(
                events.len(),
                1,
                "subtype={subtype} should yield one Error event"
            );
            assert!(matches!(events[0], RuntimeEvent::Error { .. }));
        }
    }
}
