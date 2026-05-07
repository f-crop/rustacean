#![allow(clippy::missing_errors_doc, clippy::missing_panics_doc)]

mod broadcaster;
mod config;
mod envelope;
mod errors;
mod stream;

pub use broadcaster::PerTenantBroadcaster;
pub use config::SseConfig;
pub use envelope::{EventId, SseEnvelope};
pub use errors::SseError;
pub use stream::EventStream;

// Re-export TenantId so callers don't need a direct rb-schemas dep just for the bus.
pub use rb_schemas::TenantId;

use std::sync::Arc;

// ---------------------------------------------------------------------------
// EventBus
// ---------------------------------------------------------------------------

/// Process-global SSE event bus.
///
/// Cheap to clone — backed by an [`Arc`] internally.
/// Wire one instance into `AppState` and call [`EventBus::publish`] from the
/// Kafka consumer task; call [`EventBus::subscribe`] from the SSE route.
#[derive(Clone)]
pub struct EventBus(Arc<PerTenantBroadcaster>);

impl EventBus {
    /// Create a new bus with the given configuration.
    #[must_use]
    pub fn new(cfg: SseConfig) -> Self {
        Self(Arc::new(PerTenantBroadcaster::new(cfg)))
    }

    /// Serialize `event: E` as JSON and broadcast it to all subscribers of `tenant`.
    ///
    /// Serialisation failures are logged and silently dropped — the consumer loop
    /// must not crash due to a bad payload.
    pub fn publish<E: serde::Serialize>(&self, tenant: &TenantId, name: &str, event: &E) {
        match serde_json::to_string(event) {
            Ok(data) => self.0.publish(tenant, name, data),
            Err(e) => tracing::error!("rb-sse: failed to serialize event: {e}"),
        }
    }

    /// Broadcast pre-serialised JSON data to all subscribers of `tenant`.
    ///
    /// Prefer this variant from the Kafka consumer to avoid double-serialisation.
    pub fn publish_raw(&self, tenant: &TenantId, name: &str, data: String) {
        self.0.publish(tenant, name, data);
    }

    /// Subscribe to the live event stream for `tenant`, filtering to only emit
    /// events whose `data` JSON contains `"session_id": "<session_id>"`.
    ///
    /// If `last_event_id` is provided and found in the 5-minute ring buffer,
    /// the stream first replays all events after that ID before switching to live.
    /// If not found (too old or unknown), a `stream-reset` event is emitted and
    /// the browser should perform a full state re-fetch.
    #[must_use]
    pub fn subscribe_session(
        &self,
        tenant: &TenantId,
        session_id: &uuid::Uuid,
        last_event_id: Option<&EventId>,
    ) -> EventStream {
        EventStream::new_filtered(
            Arc::clone(&self.0),
            tenant,
            Some(*session_id),
            last_event_id,
            &SseConfig::default(),
        )
    }
    ///
    /// The returned [`EventStream`] implements both [`futures::Stream`] (for tests)
    /// and [`axum::response::IntoResponse`] (for handlers).
    #[must_use]
    pub fn subscribe(&self, tenant: &TenantId, last_event_id: Option<&EventId>) -> EventStream {
        EventStream::new(
            Arc::clone(&self.0),
            tenant,
            last_event_id,
            &SseConfig::default(),
        )
    }

    /// Subscribe using an explicit [`SseConfig`] (e.g. to customise keep-alive interval).
    #[must_use]
    pub fn subscribe_with_cfg(
        &self,
        tenant: &TenantId,
        last_event_id: Option<&EventId>,
        cfg: &SseConfig,
    ) -> EventStream {
        EventStream::new(Arc::clone(&self.0), tenant, last_event_id, cfg)
    }
}

// ---------------------------------------------------------------------------
// Test helpers (always exported; internal use only)
// ---------------------------------------------------------------------------

/// Low-level test helpers that bypass the axum response layer.
///
/// Gated behind the `test-util` feature (REQ-MD-02) — not part of the default
/// public surface.  Enable via `rb-sse = { …, features = ["test-util"] }` in
/// `[dev-dependencies]`, or pass `--features test-util` to `cargo test`.
#[cfg(any(test, feature = "test-util"))]
pub mod testing {
    use std::sync::Arc;

    use tokio::sync::broadcast;

    use super::{EventBus, EventId, SseEnvelope, TenantId};

    /// Subscribe to the raw broadcast channel and replay queue without the
    /// axum response wrapper.  Use this in unit tests to assert event content.
    #[must_use]
    pub fn raw_subscribe(
        bus: &EventBus,
        tenant: &TenantId,
        last_event_id: Option<&EventId>,
    ) -> (broadcast::Receiver<Arc<SseEnvelope>>, Vec<Arc<SseEnvelope>>) {
        bus.0.subscribe_raw(tenant, last_event_id)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publish_and_receive_via_testing_api() {
        let bus = EventBus::new(SseConfig::default());
        let tenant = TenantId::new();

        let (mut rx, _) = testing::raw_subscribe(&bus, &tenant, None);
        bus.publish_raw(&tenant, "ping", r#"{"ok":true}"#.to_owned());

        let env = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        assert_eq!(env.event, "ping");
        assert_eq!(env.data, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn serialize_publish_reaches_subscriber() {
        #[derive(serde::Serialize)]
        struct Payload {
            value: u32,
        }

        let bus = EventBus::new(SseConfig::default());
        let tenant = TenantId::new();
        let (mut rx, _) = testing::raw_subscribe(&bus, &tenant, None);

        bus.publish(&tenant, "data", &Payload { value: 42 });

        let env = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
            .await
            .expect("timeout")
            .expect("closed");

        let v: serde_json::Value = serde_json::from_str(&env.data).unwrap();
        assert_eq!(v["value"], 42);
    }
}
