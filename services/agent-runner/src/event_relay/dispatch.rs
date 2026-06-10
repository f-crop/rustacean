//! Background flush loop for the event relay.
//!
//! Extracted from `event_relay` to keep that module under the 600-line cap.
//! Contains [`flush_loop`], which drains the ring buffer and POSTs batches to
//! control-api, grouped by session.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;

use super::RelayItem;
use super::retry::post_with_retry;

// ---------------------------------------------------------------------------
// Flush loop
// ---------------------------------------------------------------------------

pub(super) async fn flush_loop(
    buffer: Arc<Mutex<VecDeque<RelayItem>>>,
    notify: Arc<Notify>,
    http_client: reqwest::Client,
    control_api_base: String,
    batch_size: usize,
    flush_interval: Duration,
) {
    let mut timer = tokio::time::interval(flush_interval);
    timer.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Consume the immediate first tick so the interval starts counting from now.
    timer.tick().await;

    loop {
        // Wake on whichever fires first: timer (soft flush) or size trigger (hard flush).
        let size_triggered = tokio::select! {
            _ = timer.tick() => false,
            () = notify.notified() => {
                buffer.lock().expect("relay buffer lock poisoned").len() >= batch_size
            }
        };

        // On a size-triggered wake the batch is guaranteed full; proceed.
        // On a timer-triggered wake skip if the buffer is empty.
        if !size_triggered
            && buffer
                .lock()
                .expect("relay buffer lock poisoned")
                .is_empty()
        {
            continue;
        }

        let batch: Vec<RelayItem> = {
            let mut buf = buffer.lock().expect("relay buffer lock poisoned");
            let take = buf.len().min(batch_size);
            buf.drain(..take).collect()
        };

        if batch.is_empty() {
            continue;
        }

        // Group items by session so we POST once per session per batch.
        let mut by_session: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, item) in batch.iter().enumerate() {
            by_session
                .entry(item.session_id.clone())
                .or_default()
                .push(idx);
        }

        for (session_id, indices) in by_session {
            let items: Vec<&RelayItem> = indices.iter().map(|&i| &batch[i]).collect();
            flush_session(&http_client, &control_api_base, &session_id, &items).await;
        }
    }
}

async fn flush_session(
    client: &reqwest::Client,
    control_api_base: &str,
    session_id: &str,
    items: &[&RelayItem],
) {
    let tenant_id = &items[0].tenant_id;
    let url = format!("{control_api_base}/internal/agent/sessions/{session_id}/events");

    // Serialize each RuntimeEvent directly so the body matches IngestEventsRequest.events:
    // Vec<RuntimeEvent> with serde tag {"type": "text", "text": "..."}
    // turn_ids is a parallel array; None entries are serialized as JSON null.
    let mut events_body: Vec<serde_json::Value> = Vec::with_capacity(items.len());
    let mut turn_ids: Vec<serde_json::Value> = Vec::with_capacity(items.len());
    for item in items {
        match serde_json::to_value(&item.event) {
            Ok(v) => {
                events_body.push(v);
                turn_ids.push(match item.turn_id {
                    Some(id) => serde_json::Value::String(id.to_string()),
                    None => serde_json::Value::Null,
                });
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    seq = item.seq,
                    error = %e,
                    "relay: failed to serialize event — skipping"
                );
            }
        }
    }

    if events_body.is_empty() {
        return;
    }

    let event_count = events_body.len();
    let body = serde_json::json!({
        "tenant_id": tenant_id,
        "events": events_body,
        "turn_ids": turn_ids,
    });

    post_with_retry(client, &url, &body, session_id, event_count).await;
}
