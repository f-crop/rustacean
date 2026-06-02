use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use metrics::counter;
use rb_schemas::{AgentEvent, AgentEventKind, TenantId};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::Instrument;

use super::SessionHandle;

/// Spawns a background task that waits for the child process to exit naturally
/// and then transitions the session to `terminated` (exit 0) or `failed`
/// (non-zero exit).
///
/// Race safety: the task removes the session from `sessions` as its first
/// write action.  `terminate_session` also removes from `sessions` first.
/// Whichever removes first owns the cleanup; the other returns without action.
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(super) fn spawn_natural_exit_handler(
    session_id: String,
    tenant_id: TenantId,
    process: Arc<Mutex<crate::adapters::AgentProcess>>,
    sessions: Arc<Mutex<HashMap<String, SessionHandle>>>,
    seq_counters: Arc<Mutex<HashMap<String, i64>>>,
    seq_timestamps: Arc<Mutex<HashMap<String, Instant>>>,
    control_api_base: String,
    http_client: reqwest::Client,
    event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    tenant_session_counts: Arc<Mutex<HashMap<TenantId, usize>>>,
) -> JoinHandle<()> {
    let span = tracing::info_span!("natural_exit_handler", session_id = %session_id);

    tokio::spawn(
        async move {
            // Wait for the process to exit naturally (releases lock before HTTP calls).
            let exit_status = {
                let mut proc = process.lock().await;
                proc.child.wait().await
            };

            // Race check: try to claim ownership of cleanup by removing from the
            // sessions map.  If terminate_session already removed it, bail out.
            let Some(handle) = ({
                let mut map = sessions.lock().await;
                map.remove(&session_id)
            }) else {
                tracing::debug!(
                    session_id = %session_id,
                    "natural exit: session already removed by explicit terminate"
                );
                return;
            };

            // We own the cleanup.  Abort I/O handlers (wait_handle = self; dropping
            // it does not abort in tokio, so no self-abort risk).
            handle.stdout_handle.abort();
            handle.stderr_handle.abort();

            // Clean up seq-counter state.
            {
                let mut counters = seq_counters.lock().await;
                let mut timestamps = seq_timestamps.lock().await;
                counters.remove(&session_id);
                timestamps.remove(&session_id);
            }

            // Decrement per-tenant session count (S2 / ADR-013 §4.3).
            {
                let mut counts = tenant_session_counts.lock().await;
                if let Some(n) = counts.get_mut(&tenant_id) {
                    *n = n.saturating_sub(1);
                }
            }

            let exit_code = match exit_status {
                Ok(status) => status.code().unwrap_or(-1),
                Err(_) => -1,
            };
            let duration_ms =
                i64::try_from(handle.start_time.elapsed().as_millis()).unwrap_or(i64::MAX);
            let final_status = if exit_code == 0 {
                "terminated"
            } else {
                // Emit crash metric so operators can alert on `runtime_crashed` (ADR-013 §4.4).
                counter!("rb_session_failed_total", "error_kind" => "runtime_crashed").increment(1);
                "failed"
            };
            let error_msg = (exit_code != 0).then(|| {
                format!("error_kind=runtime_crashed: process exited with code {exit_code}")
            });

            let Ok(validated_id) = uuid::Uuid::parse_str(&session_id) else {
                tracing::warn!(
                    session_id = %session_id,
                    "natural exit: rejected non-UUID session_id"
                );
                return;
            };

            // Update session status in control-api.
            let status_url =
                format!("{control_api_base}/internal/agent/sessions/{validated_id}/status");
            let body = serde_json::json!({
                "status": final_status,
                "pid": serde_json::Value::Null,
                "exit_code": exit_code,
                "error": error_msg,
                "tenant_id": tenant_id.to_string(),
            });
            if let Err(e) = http_client.patch(&status_url).json(&body).send().await {
                tracing::warn!(
                    session_id = %session_id,
                    "natural exit: failed to update session status: {e}"
                );
            }

            // Revoke session-scoped API key.
            let revoke_url =
                format!("{control_api_base}/internal/agent/sessions/{validated_id}/api-key");
            if let Err(e) = http_client.delete(&revoke_url).send().await {
                tracing::warn!(
                    session_id = %session_id,
                    "natural exit: failed to revoke API key: {e}"
                );
            }

            // Emit Terminated lifecycle event so the SSE stream closes cleanly.
            let payload = serde_json::json!({
                "exit_code": exit_code,
                "duration_ms": duration_ms,
                "reason": "natural_exit",
            });
            let event = AgentEvent {
                tenant_id: tenant_id.to_string(),
                session_id: session_id.clone(),
                seq: super::TERMINATED_SEQ,
                kind: AgentEventKind::Terminated.into(),
                payload: payload.to_string(),
                emitted_at_ms: chrono::Utc::now().timestamp_millis(),
            };
            if tokio::time::timeout(
                Duration::from_secs(5),
                event_sender.send((tenant_id, event)),
            )
            .await
            .is_err()
            {
                tracing::warn!(
                    session_id = %session_id,
                    "Event channel full, dropped natural exit event"
                );
                counter!("rb_agent_events_dropped_total", "reason" => "channel_full").increment(1);
            }

            tracing::info!(
                session_id = %session_id,
                exit_code = exit_code,
                duration_ms = duration_ms,
                "Session completed naturally"
            );
        }
        .instrument(span),
    )
}
