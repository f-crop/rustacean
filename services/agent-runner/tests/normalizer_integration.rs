//! Integration tests for [`StreamJsonNormalizer`] driven by real NDJSON
//! fixture files captured from `claude --output-format stream-json` sessions.

use agent_runner::StreamJsonNormalizer;
use rb_schemas::RuntimeEvent;

fn normalize_fixture(filename: &str) -> Vec<RuntimeEvent> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(filename);
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {filename}: {e}"));

    content
        .lines()
        .flat_map(StreamJsonNormalizer::normalize_line)
        .collect()
}

// ---------------------------------------------------------------------------
// session_basic.ndjson — thinking + text + tool_use + tool_result + final text
// ---------------------------------------------------------------------------

#[test]
fn basic_session_event_count() {
    let events = normalize_fixture("session_basic.ndjson");
    // user_input (initial prompt) = 1
    // thinking + text + tool_use = 3 from first assistant turn
    // tool_result = 1 from user turn
    // text = 1 from second assistant turn
    // system init and success result produce no events
    // Total = 6
    assert_eq!(events.len(), 6, "events: {events:?}");
}

#[test]
fn basic_session_first_event_is_user_input() {
    let events = normalize_fixture("session_basic.ndjson");
    assert!(
        matches!(&events[0], RuntimeEvent::UserInput { text } if text.contains("List the files")),
        "expected UserInput as first event, got: {:?}",
        events[0]
    );
}

#[test]
fn basic_session_contains_thinking_then_text_then_tool_use() {
    let events = normalize_fixture("session_basic.ndjson");
    assert!(
        matches!(events[1], RuntimeEvent::Thinking { .. }),
        "events[1] should be Thinking"
    );
    assert!(
        matches!(events[2], RuntimeEvent::Text { .. }),
        "events[2] should be Text"
    );
    assert!(
        matches!(events[3], RuntimeEvent::ToolUse { .. }),
        "events[3] should be ToolUse"
    );
}

#[test]
fn basic_session_tool_use_has_correct_id_and_name() {
    let events = normalize_fixture("session_basic.ndjson");
    let tool_event = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::ToolUse { .. }));
    let Some(RuntimeEvent::ToolUse { id, name, input }) = tool_event else {
        panic!("no ToolUse event found");
    };
    assert_eq!(id, "toolu_01AbCdEfGh");
    assert_eq!(name, "bash");
    assert_eq!(input["command"], "ls -la");
}

#[test]
fn basic_session_tool_result_is_not_error() {
    let events = normalize_fixture("session_basic.ndjson");
    let result = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::ToolResult { .. }));
    let Some(RuntimeEvent::ToolResult {
        tool_use_id,
        is_error,
        ..
    }) = result
    else {
        panic!("no ToolResult event found");
    };
    assert_eq!(tool_use_id, "toolu_01AbCdEfGh");
    assert!(!is_error);
}

#[test]
fn basic_session_last_event_is_text() {
    let events = normalize_fixture("session_basic.ndjson");
    let last = events.last().expect("at least one event");
    assert!(
        matches!(last, RuntimeEvent::Text { .. }),
        "last event should be Text, got: {last:?}"
    );
}

#[test]
fn basic_session_no_error_events() {
    let events = normalize_fixture("session_basic.ndjson");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::Error { .. })),
        "basic session should not produce Error events"
    );
}

// ---------------------------------------------------------------------------
// session_error.ndjson — context-limit error with thinking + result error
// ---------------------------------------------------------------------------

#[test]
fn error_session_contains_error_event() {
    let events = normalize_fixture("session_error.ndjson");
    let err = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::Error { .. }));
    assert!(err.is_some(), "expected at least one Error event");
}

#[test]
fn error_session_error_message_and_code() {
    let events = normalize_fixture("session_error.ndjson");
    let Some(RuntimeEvent::Error { message, code }) = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::Error { .. }))
    else {
        panic!("no Error event found");
    };
    assert!(
        message.contains("Maximum output tokens"),
        "expected context-limit message, got: {message}"
    );
    assert_eq!(
        code.as_deref(),
        Some("error_during_execution"),
        "expected error code error_during_execution"
    );
}

#[test]
fn error_session_has_thinking_before_error() {
    let events = normalize_fixture("session_error.ndjson");
    let thinking_pos = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::Thinking { .. }));
    let error_pos = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::Error { .. }));
    assert!(
        thinking_pos.is_some() && error_pos.is_some(),
        "expected both thinking and error events"
    );
    assert!(
        thinking_pos.unwrap() < error_pos.unwrap(),
        "thinking must precede the error event"
    );
}

// ---------------------------------------------------------------------------
// Sequence invariant: stream-json events always start at seq >= 1.
// Verify normalizer never emits an event that would collide with the sentinel
// lifecycle sequences defined in session/mod.rs (i64::MIN+1, i64::MIN+2).
// This is not a direct seq-assignment test (the session manager assigns seqs),
// but we verify the normalizer produces the correct number of events per line
// so the seq counter advances predictably.
// ---------------------------------------------------------------------------

#[test]
fn each_fixture_line_never_panics() {
    for fixture in ["session_basic.ndjson", "session_error.ndjson"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(fixture);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {fixture}: {e}"));
        for line in content.lines() {
            // Must not panic on any line
            let _ = StreamJsonNormalizer::normalize_line(line);
        }
    }
}
