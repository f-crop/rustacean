//! In-process Kafka bus shim for unit tests.
//!
//! Available when `#[cfg(any(test, feature = "test-util"))]`.
//! Provides `InProcessBus`, `TestProducer<E>`, and `TestConsumer<E>` — the
//! same publish/consume interface as the production types but backed by
//! tokio broadcast channels instead of a real Kafka broker.

use std::{
    collections::{HashMap, HashSet},
    marker::PhantomData,
    sync::{Arc, Mutex},
};

use metrics::{counter, histogram};
use prost::Message as ProstMessage;
use tokio::sync::{Mutex as AsyncMutex, broadcast};
use uuid::Uuid;

use crate::{
    dlq::dlq_topic,
    envelope::{
        DeliveryReport, EventEnvelope, HEADER_ATTEMPT, HEADER_BLOB_REF, HEADER_CREATED_AT_MS,
        HEADER_EVENT_ID, HEADER_SCHEMA_VERSION, HEADER_TENANT_ID, HEADER_TRACEPARENT,
        HEADER_TRACESTATE,
    },
    errors::KafkaError,
    headers::{KafkaHeaderExtractor, is_valid_traceparent},
    producer::decode_envelope,
};

/// `OTel` `Injector` backed by a `Vec<(String, String)>` for use in the test double.
struct VecInjector(Vec<(String, String)>);

impl opentelemetry::propagation::Injector for VecInjector {
    fn set(&mut self, key: &str, value: String) {
        self.0.push((key.to_owned(), value));
    }
}

/// A raw Kafka-message-shaped payload passed through the in-process bus.
#[derive(Clone, Debug)]
pub struct RawMessage {
    pub key: Vec<u8>,
    pub payload: Vec<u8>,
    pub headers: Vec<(String, String)>,
    pub topic: String,
}

/// Shared in-process Kafka bus. Create one per test; share via `Arc<InProcessBus>`.
#[derive(Clone)]
pub struct InProcessBus {
    channels: Arc<Mutex<HashMap<String, broadcast::Sender<RawMessage>>>>,
}

impl InProcessBus {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Returns the broadcast sender for a topic, creating it if absent.
    fn sender_for(&self, topic: &str) -> broadcast::Sender<RawMessage> {
        let mut guard = self.channels.lock().expect("bus lock poisoned");
        guard
            .entry(topic.to_owned())
            .or_insert_with(|| {
                let (tx, _) = broadcast::channel(1024);
                tx
            })
            .clone()
    }

    /// Subscribe to a topic; returns a receiver for all messages published after this call.
    #[must_use]
    pub fn subscribe(&self, topic: &str) -> broadcast::Receiver<RawMessage> {
        self.sender_for(topic).subscribe()
    }

    /// Publish a raw message to a topic.
    pub fn publish_raw(&self, msg: RawMessage) {
        let sender = self.sender_for(&msg.topic.clone());
        // Ignore send errors if no active receivers yet.
        let _ = sender.send(msg);
    }

    /// Create a typed producer backed by this bus.
    #[must_use]
    pub fn producer<E: ProstMessage>(&self) -> TestProducer<E> {
        TestProducer {
            bus: Arc::new(self.clone()),
            seen: Arc::new(Mutex::new(HashSet::new())),
            _phantom: PhantomData,
        }
    }

    /// Create a typed consumer subscribed to `topic`.
    #[must_use]
    pub fn consumer<E: ProstMessage + Default>(&self, topic: &str) -> TestConsumer<E> {
        let receiver = self.subscribe(topic);
        TestConsumer {
            bus: Arc::new(self.clone()),
            topic: topic.to_owned(),
            receiver: Arc::new(AsyncMutex::new(receiver)),
            _phantom: PhantomData,
        }
    }
}

impl Default for InProcessBus {
    fn default() -> Self {
        Self {
            channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

// ── TestProducer ─────────────────────────────────────────────────────────────

/// Typed in-process producer. `publish()` is idempotent on `event_id`.
pub struct TestProducer<E: ProstMessage> {
    bus: Arc<InProcessBus>,
    /// Set of event IDs already published; second publish of the same ID is a no-op.
    seen: Arc<Mutex<HashSet<Uuid>>>,
    _phantom: PhantomData<E>,
}

#[allow(clippy::unused_async)]
impl<E: ProstMessage> TestProducer<E> {
    pub async fn publish(
        &self,
        topic: &str,
        key: &[u8],
        envelope: EventEnvelope<E>,
    ) -> Result<DeliveryReport, KafkaError> {
        // Idempotency: skip duplicate event_ids.
        {
            let mut seen = self.seen.lock().expect("seen lock poisoned");
            if !seen.insert(envelope.event_id) {
                return Ok(DeliveryReport {
                    topic: topic.to_owned(),
                    partition: 0,
                    offset: -1,
                });
            }
        }

        let created_at = envelope.created_at;
        // Mirror production build_headers: explicit trace_context wins; otherwise inject
        // the active OTel span so test-double headers stay wire-compatible with prod.
        let mut headers = envelope_to_headers(&envelope);
        if envelope.trace_context.is_none() {
            let mut injector = VecInjector(Vec::new());
            opentelemetry::global::get_text_map_propagator(|prop| {
                prop.inject(&mut injector);
            });
            headers.extend(injector.0);
        }
        let payload = envelope.payload.encode_to_vec();

        self.bus.publish_raw(RawMessage {
            key: key.to_vec(),
            payload,
            headers,
            topic: topic.to_owned(),
        });

        counter!(
            "rb_kafka_messages_total",
            "op" => "produce",
            "outcome" => "ok",
            "topic" => topic.to_owned()
        )
        .increment(1);
        #[allow(clippy::cast_precision_loss)]
        let e2e_secs = (chrono::Utc::now() - created_at).num_milliseconds().max(0) as f64 / 1_000.0;
        histogram!("rb_kafka_e2e_latency_seconds", "topic" => topic.to_owned()).record(e2e_secs);

        Ok(DeliveryReport {
            topic: topic.to_owned(),
            partition: 0,
            offset: 0,
        })
    }
}

// ── TestConsumer ─────────────────────────────────────────────────────────────

/// Typed in-process consumer. `next()` decodes each `RawMessage` from the bus.
pub struct TestConsumer<E: ProstMessage + Default> {
    bus: Arc<InProcessBus>,
    topic: String,
    receiver: Arc<AsyncMutex<broadcast::Receiver<RawMessage>>>,
    _phantom: PhantomData<E>,
}

#[allow(clippy::unused_async)]
impl<E: ProstMessage + Default> TestConsumer<E> {
    pub async fn next(&self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        // review-checklist-ok: async-guard — tokio::sync::Mutex is designed for cross-await holding
        let mut rx = self.receiver.lock().await;
        match rx.recv().await {
            Ok(msg) => {
                let topic = msg.topic.clone();

                // _cx_guard scoped to decode only: the guard must not be held across
                // any .await point (nack_to_dlq below). OTel context is a no-op in
                // tests without a propagator, so scoping to the sync call is safe.
                let result = {
                    let extractor = KafkaHeaderExtractor(&msg.headers);
                    let _cx_guard = opentelemetry::global::get_text_map_propagator(|prop| {
                        prop.extract(&extractor)
                    })
                    .attach();
                    decode_envelope(&msg.payload, &msg.headers, &msg.topic, 0, 0)
                    // _cx_guard dropped here
                };

                // Malformed traceparent: auto-DLQ and return error, matching Consumer::next().
                // trace_context is cleared before nack so the DLQ record doesn't
                // re-trigger validation when a downstream DLQ consumer reads it.
                let result = match result {
                    Ok(mut env) => {
                        let malformed_tp = env.trace_context.as_ref().and_then(|tc| {
                            if !tc.traceparent.is_empty() && !is_valid_traceparent(&tc.traceparent)
                            {
                                Some(tc.traceparent.clone())
                            } else {
                                None
                            }
                        });
                        if let Some(tp) = malformed_tp {
                            env.trace_context = None;
                            let _ = self.nack_to_dlq(&env, "invalid-traceparent").await;
                            counter!(
                                "rb_kafka_messages_total",
                                "op" => "consume",
                                "outcome" => "err",
                                "topic" => topic.clone()
                            )
                            .increment(1);
                            return Some(Err(KafkaError::InvalidTraceparent(tp)));
                        }
                        Ok(env)
                    }
                    Err(e) => Err(e),
                };

                match &result {
                    Ok(env) => {
                        counter!(
                            "rb_kafka_messages_total",
                            "op" => "consume",
                            "outcome" => "ok",
                            "topic" => topic.clone()
                        )
                        .increment(1);
                        #[allow(clippy::cast_precision_loss)]
                        let age_secs = (chrono::Utc::now() - env.created_at)
                            .num_milliseconds()
                            .max(0) as f64
                            / 1_000.0;
                        histogram!(
                            "rb_kafka_consume_lag_seconds",
                            "topic" => topic.clone()
                        )
                        .record(age_secs);
                    }
                    Err(_) => {
                        counter!(
                            "rb_kafka_messages_total",
                            "op" => "consume",
                            "outcome" => "err",
                            "topic" => topic.clone()
                        )
                        .increment(1);
                    }
                }
                Some(result)
            }
            Err(broadcast::error::RecvError::Closed) => None,
            Err(broadcast::error::RecvError::Lagged(_)) => Some(Err(KafkaError::ConsumerLag)),
        }
    }

    /// Commit is a no-op in the in-process bus; always succeeds.
    pub async fn commit(&self, _env: &EventEnvelope<E>) -> Result<(), KafkaError> {
        Ok(())
    }

    /// Routes the message to `{topic}.dlq` on the in-process bus.
    pub async fn nack_to_dlq(
        &self,
        env: &EventEnvelope<E>,
        reason: &str,
    ) -> Result<(), KafkaError> {
        use crate::envelope::{HEADER_DLQ_AT, HEADER_DLQ_REASON};
        use chrono::Utc;

        let mut headers = envelope_to_headers(env);
        headers.push((HEADER_DLQ_REASON.to_owned(), reason.to_owned()));
        headers.push((
            HEADER_DLQ_AT.to_owned(),
            Utc::now().timestamp_millis().to_string(),
        ));

        let payload = env.payload.encode_to_vec();
        let key = env.tenant_id.to_string().into_bytes();
        let dlq = dlq_topic(&self.topic);

        self.bus.publish_raw(RawMessage {
            key,
            payload,
            headers,
            topic: dlq,
        });

        counter!(
            "rb_kafka_dlq_total",
            "topic" => self.topic.clone(),
            "reason" => reason.to_owned()
        )
        .increment(1);

        Ok(())
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

#[allow(clippy::used_underscore_binding)]
fn envelope_to_headers<E: ProstMessage>(env: &EventEnvelope<E>) -> Vec<(String, String)> {
    let mut h = vec![
        (HEADER_TENANT_ID.to_owned(), env.tenant_id.to_string()),
        (HEADER_EVENT_ID.to_owned(), env.event_id.to_string()),
        (
            HEADER_SCHEMA_VERSION.to_owned(),
            env.schema_version.as_str().to_owned(),
        ),
        (HEADER_ATTEMPT.to_owned(), env._meta.attempt.to_string()),
        (
            HEADER_CREATED_AT_MS.to_owned(),
            env.created_at.timestamp_millis().to_string(),
        ),
    ];
    if let Some(ref blob) = env.blob_ref {
        h.push((HEADER_BLOB_REF.to_owned(), blob.clone()));
    }
    if let Some(ref tc) = env.trace_context {
        if !tc.traceparent.is_empty() {
            h.push((HEADER_TRACEPARENT.to_owned(), tc.traceparent.clone()));
        }
        if !tc.tracestate.is_empty() {
            h.push((HEADER_TRACESTATE.to_owned(), tc.tracestate.clone()));
        }
    }
    h
}
