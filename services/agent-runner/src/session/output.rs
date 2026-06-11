use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use rb_schemas::{AgentEvent, AgentEventKind, TenantId};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::Instrument;

use crate::adapters::RuntimeAdapter;

use super::{redact, seq};

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_output_handlers(
    session_id: String,
    tenant_id: TenantId,
    stdout: ChildStdout,
    stderr: ChildStderr,
    event_sender: tokio::sync::mpsc::Sender<(TenantId, AgentEvent)>,
    adapter: Box<dyn RuntimeAdapter>,
    live_token: String,
    current_turn_id: Arc<RwLock<Option<uuid::Uuid>>>,
    seq_counters: Arc<Mutex<HashMap<String, i64>>>,
    seq_timestamps: Arc<Mutex<HashMap<String, Instant>>>,
    relay_sender: &agent_runner::EventSender,
) -> (JoinHandle<()>, JoinHandle<()>) {
    let sid_stdout = session_id.clone();
    let span_out = tracing::info_span!("stdout_handler", session_id = %sid_stdout);
    let live_token_stdout = live_token.clone();

    let stdout_handle = tokio::spawn(
        {
            let es = event_sender.clone();
            let seq_counters = seq_counters.clone();
            let seq_timestamps = seq_timestamps.clone();
            let relay_sender = relay_sender.clone();
            let adapter = adapter;
            async move {
                let reader = BufReader::new(stdout);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let seq = seq::next_seq(&seq_counters, &seq_timestamps, &sid_stdout).await;
                    if let Some(parsed) = adapter.parse_stdout_line(&line) {
                        // Redact payload before relay (ADR-013 §6.3); fail-closed: panic → drop line.
                        let Some(redacted_payload) = redact::redact_guarded(
                            std::panic::AssertUnwindSafe(|| {
                                rb_secrets::redact_with_token(
                                    &parsed.payload,
                                    Some(&live_token_stdout),
                                )
                                .into_owned()
                            }),
                            &sid_stdout,
                        ) else {
                            continue;
                        };
                        let event = AgentEvent {
                            tenant_id: tenant_id.to_string(),
                            session_id: sid_stdout.clone(),
                            seq,
                            kind: AgentEventKind::Stdout.into(),
                            payload: redacted_payload,
                            emitted_at_ms: chrono::Utc::now().timestamp_millis(),
                        };
                        if let Err(e) = es.try_send((tenant_id, event)) {
                            tracing::error!(session_id = %sid_stdout, error = %e, "Failed to send stdout event (channel full or closed)");
                        }
                        // Redact raw line before SSE/DB relay (ADR-013 §6.3); fail-closed.
                        let Some(redacted_line) = redact::redact_guarded(
                            std::panic::AssertUnwindSafe(|| {
                                rb_secrets::redact_with_token(&line, Some(&live_token_stdout))
                                    .into_owned()
                            }),
                            &sid_stdout,
                        ) else {
                            continue;
                        };
                        // Snapshot the current turn_id atomically for this relay item.
                        let turn_id = current_turn_id
                            .read()
                            .ok()
                            .and_then(|g| *g);
                        agent_runner::relay_stdout_events(
                            &relay_sender,
                            &sid_stdout,
                            &tenant_id.to_string(),
                            seq,
                            &redacted_line,
                            turn_id,
                        );
                    }
                }
            }
        }
        .instrument(span_out),
    );

    let seq_counters2 = seq_counters;
    let seq_timestamps2 = seq_timestamps;
    let sid_err = session_id;
    let span_err = tracing::info_span!("stderr_handler", session_id = %sid_err);

    let stderr_handle = tokio::spawn(
        async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let seq = seq::next_seq(&seq_counters2, &seq_timestamps2, &sid_err).await;
                // Redact stderr before structured logs (ADR-013 §4.2/§6.3); fail-closed.
                let Some(redacted) = redact::redact_guarded(
                    std::panic::AssertUnwindSafe(|| {
                        rb_secrets::redact_with_token(&line, Some(&live_token)).into_owned()
                    }),
                    &sid_err,
                ) else {
                    continue;
                };
                let event = AgentEvent {
                    tenant_id: tenant_id.to_string(),
                    session_id: sid_err.clone(),
                    seq,
                    kind: AgentEventKind::Stderr.into(),
                    payload: redacted,
                    emitted_at_ms: chrono::Utc::now().timestamp_millis(),
                };
                if let Err(e) = event_sender.try_send((tenant_id, event)) {
                    tracing::error!(session_id = %sid_err, error = %e, "Failed to send stderr event (channel full or closed)");
                }
            }
        }
        .instrument(span_err),
    );

    (stdout_handle, stderr_handle)
}
