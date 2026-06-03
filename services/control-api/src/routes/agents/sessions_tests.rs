use super::sessions_patch::{
    PatchSessionStatusRequest, lifecycle_event_payload, lifecycle_event_seq, lifecycle_event_type,
};
use super::*;

#[test]
fn initial_message_max_bytes_is_64kib() {
    assert_eq!(INITIAL_MESSAGE_MAX_BYTES, 65_536);
}

// ── lifecycle_event_type ──────────────────────────────────────────────────

#[test]
fn lifecycle_event_type_running() {
    assert_eq!(lifecycle_event_type("running"), Some("session.running"));
}

#[test]
fn lifecycle_event_type_failed() {
    assert_eq!(lifecycle_event_type("failed"), Some("session.failed"));
}

#[test]
fn lifecycle_event_type_terminated_maps_to_completed() {
    assert_eq!(
        lifecycle_event_type("terminated"),
        Some("session.completed")
    );
}

#[test]
fn lifecycle_event_type_other_statuses_return_none() {
    for s in ["cancelled", "terminating", "pending", "unknown"] {
        assert_eq!(
            lifecycle_event_type(s),
            None,
            "expected None for status '{s}'"
        );
    }
}

// ── lifecycle_event_seq ───────────────────────────────────────────────────

#[test]
fn lifecycle_event_seq_failed_matches_runner_error_sentinel() {
    assert_eq!(lifecycle_event_seq("failed"), i64::MIN + 1);
}

#[test]
fn lifecycle_event_seq_terminated_matches_runner_terminated_sentinel() {
    assert_eq!(lifecycle_event_seq("terminated"), i64::MIN + 2);
}

#[test]
fn lifecycle_event_seq_running_is_zero() {
    assert_eq!(lifecycle_event_seq("running"), 0);
}

#[test]
fn lifecycle_event_seq_sentinels_are_distinct() {
    assert_ne!(
        lifecycle_event_seq("failed"),
        lifecycle_event_seq("terminated")
    );
    assert_ne!(
        lifecycle_event_seq("failed"),
        lifecycle_event_seq("running")
    );
    assert_ne!(
        lifecycle_event_seq("terminated"),
        lifecycle_event_seq("running")
    );
}

// ── lifecycle_event_payload ───────────────────────────────────────────────

#[test]
fn lifecycle_event_payload_running_includes_pid() {
    let req = PatchSessionStatusRequest {
        status: "running".to_string(),
        pid: Some(12345),
        exit_code: None,
        error: None,
        tenant_id: Uuid::new_v4(),
    };
    let p = lifecycle_event_payload(&req);
    assert_eq!(p["pid"], 12345);
}

#[test]
fn lifecycle_event_payload_failed_includes_failure_reason() {
    let req = PatchSessionStatusRequest {
        status: "failed".to_string(),
        pid: None,
        exit_code: None,
        error: Some("spawn failed: no such file".to_string()),
        tenant_id: Uuid::new_v4(),
    };
    let p = lifecycle_event_payload(&req);
    assert_eq!(p["failure_reason"], "spawn failed: no such file");
}

#[test]
fn lifecycle_event_payload_failed_null_reason_when_no_error() {
    let req = PatchSessionStatusRequest {
        status: "failed".to_string(),
        pid: None,
        exit_code: None,
        error: None,
        tenant_id: Uuid::new_v4(),
    };
    let p = lifecycle_event_payload(&req);
    assert!(p["failure_reason"].is_null());
}

#[test]
fn lifecycle_event_payload_terminated_includes_exit_code() {
    let req = PatchSessionStatusRequest {
        status: "terminated".to_string(),
        pid: None,
        exit_code: Some(0),
        error: None,
        tenant_id: Uuid::new_v4(),
    };
    let p = lifecycle_event_payload(&req);
    assert_eq!(p["exit_code"], 0);
}

#[test]
fn lifecycle_event_payload_terminated_nonzero_exit_code() {
    let req = PatchSessionStatusRequest {
        status: "terminated".to_string(),
        pid: None,
        exit_code: Some(1),
        error: None,
        tenant_id: Uuid::new_v4(),
    };
    let p = lifecycle_event_payload(&req);
    assert_eq!(p["exit_code"], 1);
}
