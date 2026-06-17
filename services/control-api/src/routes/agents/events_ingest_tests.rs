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
    assert_eq!(
        event_type(&RuntimeEvent::TurnComplete {
            stop_reason: "success".into()
        }),
        "session.turn_complete"
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

#[test]
fn turn_ids_defaults_to_empty_when_absent() {
    let json_str = serde_json::to_string(&serde_json::json!({
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "events": [{"type": "text", "text": "hi"}]
    }))
    .unwrap();
    let req: IngestEventsRequest = serde_json::from_str(&json_str).unwrap();
    assert!(
        req.turn_ids.is_empty(),
        "legacy payload must default to empty turn_ids"
    );
    // out-of-bounds → None (AC-4 backward compat)
    assert_eq!(req.turn_ids.first().copied().flatten(), None);
}

#[test]
fn turn_ids_parallel_array_deserializes() {
    let tid = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let json_str = serde_json::to_string(&serde_json::json!({
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "events": [
            {"type": "text", "text": "hello"},
            {"type": "turn_complete", "stop_reason": "end_turn"}
        ],
        "turn_ids": [tid.to_string(), tid.to_string()]
    }))
    .unwrap();
    let req: IngestEventsRequest = serde_json::from_str(&json_str).unwrap();
    assert_eq!(req.turn_ids.len(), 2);
    assert_eq!(req.turn_ids[0], Some(tid));
    assert_eq!(req.turn_ids[1], Some(tid));
}

#[test]
fn turn_ids_null_entries_deserialize_to_none() {
    let tid = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let json_str = serde_json::to_string(&serde_json::json!({
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "events": [
            {"type": "user_input", "text": "hi"},
            {"type": "text", "text": "hello"}
        ],
        "turn_ids": [null, tid.to_string()]
    }))
    .unwrap();
    let req: IngestEventsRequest = serde_json::from_str(&json_str).unwrap();
    assert_eq!(req.turn_ids[0], None);
    assert_eq!(req.turn_ids[1], Some(tid));
}
