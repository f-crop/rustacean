use std::time::Duration;

use metrics::counter;
use rb_schemas::{AgentEvent, AgentEventKind, TenantId};

pub(super) async fn emit_lifecycle_event(
    tenant_id: TenantId,
    session_id: &str,
    seq: i64,
    kind: AgentEventKind,
    payload: &str,
    event_sender: &tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
) {
    let ev = AgentEvent {
        tenant_id: tenant_id.to_string(),
        session_id: session_id.to_string(),
        seq,
        kind: kind.into(),
        payload: payload.to_string(),
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    if tokio::time::timeout(Duration::from_secs(5), event_sender.send((tenant_id, ev)))
        .await
        .is_err()
    {
        tracing::warn!(session_id = %session_id, "Event channel full, dropped event");
        counter!("rb_agent_events_dropped_total", "reason" => "channel_full").increment(1);
    }
}

pub(super) async fn emit_terminated_event(
    tenant_id: TenantId,
    session_id: &str,
    exit_code: i32,
    duration_ms: i64,
    reason: &str,
    event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
) {
    let payload =
        serde_json::json!({"exit_code":exit_code,"duration_ms":duration_ms,"reason":reason});
    let ev = AgentEvent {
        tenant_id: tenant_id.to_string(),
        session_id: session_id.to_string(),
        seq: super::TERMINATED_SEQ,
        kind: AgentEventKind::Terminated.into(),
        payload: payload.to_string(),
        emitted_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    if tokio::time::timeout(Duration::from_secs(5), event_sender.send((tenant_id, ev)))
        .await
        .is_err()
    {
        tracing::warn!(session_id = %session_id, "Event channel full, dropped terminated event");
        counter!("rb_agent_events_dropped_total", "reason" => "channel_full").increment(1);
    }
}
