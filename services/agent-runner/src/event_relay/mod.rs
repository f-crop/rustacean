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

mod dispatch;
mod retry;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use metrics::counter;
use rb_schemas::RuntimeEvent;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_CAPACITY: usize = 8_000;
pub const DEFAULT_BATCH_SIZE: usize = 100;
pub const DEFAULT_FLUSH_INTERVAL_MS: u64 = 250;

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
    /// Minimum buffer length that triggers an immediate (hard) flush.
    batch_size: usize,
    notify: Arc<Notify>,
}

impl EventSender {
    /// Enqueue `item` for relay. Never blocks; evicts the oldest item if full.
    ///
    /// `notify_one()` fires only when the buffer reaches `batch_size`, so items
    /// accumulate into full batches under continuous load. Partial batches are
    /// flushed by the timer in the background flush loop.
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
        let should_notify = buf.len() >= self.batch_size;
        drop(buf);
        if should_notify {
            self.notify.notify_one();
        }
    }
}

/// Normalize `line` from agent stdout and relay all resulting [`RuntimeEvent`]s.
pub fn relay_stdout_events(
    relay_sender: &EventSender,
    session_id: &str,
    tenant_id: &str,
    seq: i64,
    line: &str,
) {
    let now_ms = Utc::now().timestamp_millis();
    for runtime_event in crate::StreamJsonNormalizer::normalize_line(line) {
        relay_sender.send(RelayItem {
            session_id: session_id.to_string(),
            tenant_id: tenant_id.to_string(),
            seq,
            event: runtime_event,
            emitted_at_ms: now_ms,
        });
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
        batch_size: config.batch_size,
        notify: Arc::clone(&notify),
    };

    tokio::spawn(dispatch::flush_loop(
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
            batch_size: DEFAULT_BATCH_SIZE,
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

    // ── Notification gating ─────────────────────────────────────────────────

    #[tokio::test]
    async fn notify_does_not_fire_below_batch_size() {
        let batch_size: usize = 3;
        let sender = EventSender {
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            capacity: 100,
            batch_size,
            notify: Arc::new(Notify::new()),
        };

        // Push batch_size - 1 items; notify must NOT fire.
        for i in 0..(batch_size - 1) {
            sender.send(make_item(i64::try_from(i).unwrap()));
        }
        let timed_out =
            tokio::time::timeout(Duration::from_millis(20), sender.notify.notified()).await;
        assert!(
            timed_out.is_err(),
            "notify_one must not fire before batch_size items are buffered"
        );
    }

    #[tokio::test]
    async fn notify_fires_exactly_when_batch_size_is_reached() {
        let batch_size: usize = 3;
        let sender = EventSender {
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            capacity: 100,
            batch_size,
            notify: Arc::new(Notify::new()),
        };

        // Fill up to the threshold.
        for i in 0..batch_size {
            sender.send(make_item(i64::try_from(i).unwrap()));
        }
        let result =
            tokio::time::timeout(Duration::from_millis(20), sender.notify.notified()).await;
        assert!(
            result.is_ok(),
            "notify_one must fire when buffer reaches batch_size"
        );
    }
}
