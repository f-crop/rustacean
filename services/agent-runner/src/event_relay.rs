//! HTTP relay that batches [`RuntimeEvent`] items and POSTs them to
//! `POST /internal/agent/sessions/{id}/events` on control-api.
//!
//! # Design
//!
//! The relay owns a shared ring buffer (`VecDeque`) protected by a `Mutex`.
//! A background task drains the buffer on two triggers:
//!
//! - **Size trigger** — batch reached `batch_size` items (hard flush, immediate).
//! - **Time trigger** — `flush_interval` elapsed (soft flush, drains whatever is queued).
//!
//! When the buffer is full, [`EventSender::send`] evicts the **oldest** item
//! before enqueuing the new one (`rb_agent_relay_dropped_total{reason="buffer_full"}`).
//! Sends never block.
//!
//! Failed POSTs to control-api are retried with exponential backoff for transient
//! errors (5xx, connection refused/timeout). 4xx errors are not retried.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use metrics::counter;
use rb_schemas::RuntimeEvent;
use tokio::sync::Notify;
use tokio::time::MissedTickBehavior;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_CAPACITY: usize = 8_000;
pub const DEFAULT_BATCH_SIZE: usize = 100;
pub const DEFAULT_FLUSH_INTERVAL_MS: u64 = 250;

const MAX_RETRY_ATTEMPTS: u32 = 5;

#[cfg(not(test))]
const BASE_RETRY_DELAY_MS: u64 = 100;
#[cfg(test)]
const BASE_RETRY_DELAY_MS: u64 = 1; // fast retries in unit tests

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A single event enqueued for HTTP relay to control-api.
#[derive(Clone)]
pub struct RelayItem {
    pub session_id: String,
    pub tenant_id: String,
    pub seq: i64,
    pub event: RuntimeEvent,
    pub emitted_at_ms: i64,
}

/// Cloneable sender handle for the relay buffer.
///
/// All sends are non-blocking. When the buffer is full the oldest item is
/// evicted and `rb_agent_relay_dropped_total{reason="buffer_full"}` is
/// incremented.
#[derive(Clone)]
pub struct EventSender {
    buffer: Arc<Mutex<VecDeque<RelayItem>>>,
    capacity: usize,
    notify: Arc<Notify>,
}

impl EventSender {
    /// Enqueue `item` for relay. Never blocks; evicts the oldest item if full.
    ///
    /// # Panics
    ///
    /// Panics if the internal buffer mutex has been poisoned, which only
    /// happens if another thread panicked while holding the lock.
    pub fn send(&self, item: RelayItem) {
        let mut buf = self.buffer.lock().expect("relay buffer lock poisoned");
        if buf.len() >= self.capacity {
            buf.pop_front();
            counter!("rb_agent_relay_dropped_total", "reason" => "buffer_full").increment(1);
        }
        buf.push_back(item);
        drop(buf);
        self.notify.notify_one();
    }
}

/// Configuration for the `EventRelay`.
pub struct RelayConfig {
    /// Maximum number of [`RelayItem`]s buffered before the oldest is evicted.
    pub capacity: usize,
    /// Maximum items to include in a single POST to control-api.
    pub batch_size: usize,
    /// Maximum time to wait before flushing a partial batch.
    pub flush_interval: Duration,
    pub control_api_base: String,
    pub http_client: reqwest::Client,
}

/// Spawn the relay flush task and return a [`EventSender`] handle.
///
/// The flush task runs until the process exits. The caller keeps the returned
/// sender alive; dropping all senders does not stop the flush task (it keeps
/// draining whatever was already enqueued).
#[must_use]
pub fn spawn(config: RelayConfig) -> EventSender {
    let buffer = Arc::new(Mutex::new(VecDeque::with_capacity(config.capacity)));
    let notify = Arc::new(Notify::new());

    let sender = EventSender {
        buffer: Arc::clone(&buffer),
        capacity: config.capacity,
        notify: Arc::clone(&notify),
    };

    tokio::spawn(flush_loop(
        Arc::clone(&buffer),
        Arc::clone(&notify),
        config.http_client,
        config.control_api_base,
        config.batch_size,
        config.flush_interval,
    ));

    sender
}

// ---------------------------------------------------------------------------
// Flush loop
// ---------------------------------------------------------------------------

async fn flush_loop(
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
    let events_body: Vec<serde_json::Value> = items
        .iter()
        .filter_map(|i| match serde_json::to_value(&i.event) {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    seq = i.seq,
                    error = %e,
                    "relay: failed to serialize event — skipping"
                );
                None
            }
        })
        .collect();

    if events_body.is_empty() {
        return;
    }

    let event_count = events_body.len();
    let body = serde_json::json!({
        "tenant_id": tenant_id,
        "events": events_body,
    });

    post_with_retry(client, &url, &body, session_id, event_count).await;
}

// ---------------------------------------------------------------------------
// HTTP POST with exponential backoff retry
// ---------------------------------------------------------------------------

async fn post_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    session_id: &str,
    event_count: usize,
) {
    for attempt in 0..MAX_RETRY_ATTEMPTS {
        if attempt > 0 {
            let delay_ms = BASE_RETRY_DELAY_MS * (1u64 << (attempt - 1));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        match client.post(url).json(body).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    session_id = %session_id,
                    event_count = event_count,
                    "EventRelay: batch flushed"
                );
                counter!("rb_agent_relay_published_total").increment(event_count as u64);
                return;
            }
            Ok(resp) if resp.status().is_server_error() => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempt + 1,
                    status = %resp.status(),
                    event_count = event_count,
                    "EventRelay: POST 5xx — will retry"
                );
                counter!("rb_agent_relay_retry_total").increment(1);
            }
            Ok(resp) => {
                // 4xx and other non-retryable responses
                tracing::warn!(
                    session_id = %session_id,
                    status = %resp.status(),
                    event_count = event_count,
                    "EventRelay: POST failed (non-retryable) — dropping batch"
                );
                counter!("rb_agent_relay_failed_total").increment(event_count as u64);
                return;
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempt + 1,
                    error = %e,
                    event_count = event_count,
                    "EventRelay: connection error — will retry"
                );
                counter!("rb_agent_relay_retry_total").increment(1);
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    event_count = event_count,
                    "EventRelay: POST error (non-retryable) — dropping batch"
                );
                counter!("rb_agent_relay_failed_total").increment(event_count as u64);
                return;
            }
        }
    }

    tracing::error!(
        session_id = %session_id,
        event_count = event_count,
        max_attempts = MAX_RETRY_ATTEMPTS,
        "EventRelay: batch dropped after exhausting all retry attempts"
    );
    counter!("rb_agent_relay_failed_total").increment(event_count as u64);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::RuntimeEvent;

    fn make_item(seq: i64) -> RelayItem {
        RelayItem {
            session_id: "sess-001".to_string(),
            tenant_id: "tenant-001".to_string(),
            seq,
            event: RuntimeEvent::Text {
                text: format!("event-{seq}"),
            },
            emitted_at_ms: 0,
        }
    }

    fn make_sender(capacity: usize) -> EventSender {
        EventSender {
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            capacity,
            notify: Arc::new(Notify::new()),
        }
    }

    fn buf_len(sender: &EventSender) -> usize {
        sender.buffer.lock().unwrap().len()
    }

    fn front_seq(sender: &EventSender) -> Option<i64> {
        sender.buffer.lock().unwrap().front().map(|i| i.seq)
    }

    fn back_seq(sender: &EventSender) -> Option<i64> {
        sender.buffer.lock().unwrap().back().map(|i| i.seq)
    }

    // ── Drop-oldest behavior ─────────────────────────────────────────────────

    #[test]
    fn drops_oldest_when_buffer_is_full() {
        let sender = make_sender(3);
        sender.send(make_item(1));
        sender.send(make_item(2));
        sender.send(make_item(3));
        assert_eq!(buf_len(&sender), 3);

        // Sending a 4th item must evict seq=1 (oldest).
        sender.send(make_item(4));
        assert_eq!(buf_len(&sender), 3, "buffer must not exceed capacity");
        assert_eq!(
            front_seq(&sender),
            Some(2),
            "oldest remaining must be seq=2"
        );
        assert_eq!(back_seq(&sender), Some(4), "newest must be seq=4");
    }

    #[test]
    fn buffer_never_exceeds_capacity_under_sustained_load() {
        let capacity = 5;
        let sender = make_sender(capacity);
        for i in 0..100 {
            sender.send(make_item(i));
        }
        assert_eq!(
            buf_len(&sender),
            capacity,
            "buffer must be capped at capacity"
        );
        // The newest `capacity` items (indices 95..=99) must be retained.
        assert_eq!(front_seq(&sender), Some(95));
        assert_eq!(back_seq(&sender), Some(99));
    }

    #[test]
    fn send_into_empty_buffer_works() {
        let sender = make_sender(10);
        sender.send(make_item(42));
        assert_eq!(buf_len(&sender), 1);
        assert_eq!(front_seq(&sender), Some(42));
    }

    #[test]
    fn capacity_one_always_retains_the_newest_item() {
        let sender = make_sender(1);
        sender.send(make_item(1));
        sender.send(make_item(2));
        sender.send(make_item(3));
        assert_eq!(buf_len(&sender), 1);
        // Only the most-recently enqueued item survives.
        assert_eq!(front_seq(&sender), Some(3));
    }

    // ── Retry behavior ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn retries_once_on_500_then_succeeds_on_2xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // First request → 500; subsequent → 204
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s1/events"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s1/events"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s1/events", server.uri());

        post_with_retry(&client, &url, &body, "s1", 0).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(
            n, 2,
            "should have made exactly 2 requests (1 failure + 1 success)"
        );
    }

    #[tokio::test]
    async fn exhausts_all_retry_attempts_on_persistent_5xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s2/events"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s2/events", server.uri());

        post_with_retry(&client, &url, &body, "s2", 5).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(
            n, MAX_RETRY_ATTEMPTS as usize,
            "must exhaust all {MAX_RETRY_ATTEMPTS} retry attempts"
        );
    }

    #[tokio::test]
    async fn does_not_retry_on_4xx_client_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s3/events"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s3/events", server.uri());

        post_with_retry(&client, &url, &body, "s3", 1).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(n, 1, "4xx must not trigger any retries");
    }

    #[tokio::test]
    async fn succeeds_on_first_attempt_makes_exactly_one_request() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s4/events"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s4/events", server.uri());

        post_with_retry(&client, &url, &body, "s4", 3).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(n, 1, "success on first attempt — no retries");
    }
}
