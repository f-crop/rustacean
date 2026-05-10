use std::{marker::PhantomData, str::FromStr as _, time::Duration};

use metrics::{counter, histogram};
use prost::Message as ProstMessage;
use rdkafka::{
    ClientConfig,
    message::{Header, OwnedHeaders},
    producer::{FutureProducer, FutureRecord},
};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::{
    config::ProducerCfg,
    envelope::{
        DeliveryReport, EnvelopeMeta, EventEnvelope, HEADER_ATTEMPT, HEADER_BLOB_REF,
        HEADER_CREATED_AT_MS, HEADER_EVENT_ID, HEADER_PROCESS_AFTER_MS, HEADER_SCHEMA_VERSION,
        HEADER_TENANT_ID, HEADER_TRACEPARENT, HEADER_TRACESTATE, SchemaVersion, TraceContext,
    },
    errors::KafkaError,
    headers::KafkaHeaderInjector,
    retry::RetryPolicy,
};

pub struct Producer<E: ProstMessage> {
    inner: FutureProducer,
    _phantom: PhantomData<E>,
}

impl<E: ProstMessage> Producer<E> {
    pub fn new(cfg: &ProducerCfg) -> Result<Self, KafkaError> {
        let producer: FutureProducer = ClientConfig::new()
            .set("bootstrap.servers", &cfg.bootstrap_servers)
            // ADR-006 §3.1 invariants — not caller-configurable.
            .set("acks", "all")
            .set("enable.idempotence", "true")
            .set("compression.type", &cfg.compression_type)
            .set("linger.ms", cfg.linger_ms.to_string())
            .set("delivery.timeout.ms", cfg.delivery_timeout_ms.to_string())
            .set(
                "queue.buffering.max.kbytes",
                cfg.queue_buffering_max_kbytes.to_string(),
            )
            .set("max.in.flight.requests.per.connection", "5")
            .create()?;
        Ok(Self {
            inner: producer,
            _phantom: PhantomData,
        })
    }

    /// Probe broker connectivity by fetching cluster metadata with a short timeout.
    ///
    /// Returns `true` if at least one broker acknowledges within `timeout`.
    /// Call this before touching the database so a bad bootstrap-server config
    /// surfaces as 503 rather than a 500 after a 30-second delivery timeout.
    pub async fn check_ready(&self, timeout: Duration) -> bool {
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || {
            // Bring rdkafka's Producer trait into scope for `client()`.
            // `as _` avoids naming it (no conflict with our Producer<E> struct).
            use rdkafka::producer::Producer as _;
            inner.client().fetch_metadata(None, timeout).is_ok()
        })
        .await
        .unwrap_or(false)
    }

    pub async fn publish(
        &self,
        topic: &str,
        key: &[u8],
        envelope: EventEnvelope<E>,
    ) -> Result<DeliveryReport, KafkaError> {
        let key_str = String::from_utf8_lossy(key);
        let produce_span = tracing::info_span!(
            "kafka.produce",
            "otel.kind" = "PRODUCER",
            "messaging.system" = "kafka",
            "messaging.destination" = %topic,
            "messaging.kafka.message_key" = %key_str,
            "rb.tenant_id" = %envelope.tenant_id,
            "rb.event_id" = %envelope.event_id,
            "rb.schema_version" = envelope.schema_version.as_str(),
        );
        let created_at = envelope.created_at;
        // in_scope: span active during synchronous header injection so the OTel
        // propagator captures produce_span context into the Kafka traceparent header.
        let (headers, payload) =
            produce_span.in_scope(|| (build_headers(&envelope), envelope.payload.encode_to_vec()));

        let record = FutureRecord::to(topic)
            .key(key)
            .payload(payload.as_slice())
            .headers(headers);

        match self
            .inner
            .send(record, Duration::from_secs(30))
            .instrument(produce_span)
            .await
        {
            Ok((partition, offset)) => {
                counter!(
                    "rb_kafka_messages_total",
                    "op" => "produce",
                    "outcome" => "ok",
                    "topic" => topic.to_owned()
                )
                .increment(1);
                #[allow(clippy::cast_precision_loss)]
                let e2e_secs =
                    (chrono::Utc::now() - created_at).num_milliseconds().max(0) as f64 / 1_000.0;
                histogram!("rb_kafka_e2e_latency_seconds", "topic" => topic.to_owned())
                    .record(e2e_secs);
                Ok(DeliveryReport {
                    topic: topic.to_owned(),
                    partition,
                    offset,
                })
            }
            Err((e, _)) => {
                counter!(
                    "rb_kafka_messages_total",
                    "op" => "produce",
                    "outcome" => "err",
                    "topic" => topic.to_owned()
                )
                .increment(1);
                Err(KafkaError::from(e))
            }
        }
    }

    /// Publish `envelope` to the retry topic for `topic`, incrementing the attempt counter.
    /// Uses `policy.process_after_ms` to set the backoff timestamp header.
    /// Returns `Err(KafkaError::MaxRetriesExceeded)` when the policy considers this attempt
    /// terminal (i.e. `policy.is_terminal(next_attempt)`).
    #[allow(clippy::used_underscore_binding)]
    pub async fn publish_retry(
        &self,
        topic: &str,
        retry_topic: &str,
        key: &[u8],
        mut envelope: EventEnvelope<E>,
        policy: &RetryPolicy,
    ) -> Result<DeliveryReport, KafkaError> {
        let created_at = envelope.created_at;
        let next_attempt = envelope._meta.attempt + 1;
        envelope._meta.attempt = next_attempt;

        if policy.is_terminal(next_attempt) {
            return Err(KafkaError::MaxRetriesExceeded);
        }

        let due_ms = policy.process_after_ms(next_attempt);
        let headers = build_headers_with_retry(&envelope, due_ms);
        let payload = envelope.payload.encode_to_vec();

        let record = FutureRecord::to(retry_topic)
            .key(key)
            .payload(payload.as_slice())
            .headers(headers);

        let (partition, offset) = self
            .inner
            .send(record, Duration::from_secs(30))
            .await
            .map_err(|(e, _)| KafkaError::from(e))?;

        counter!(
            "rb_kafka_retry_total",
            "topic" => topic.to_owned(),
            "attempt" => next_attempt.to_string()
        )
        .increment(1);
        #[allow(clippy::cast_precision_loss)]
        let e2e_secs = (chrono::Utc::now() - created_at).num_milliseconds().max(0) as f64 / 1_000.0;
        histogram!("rb_kafka_e2e_latency_seconds", "topic" => topic.to_owned()).record(e2e_secs);

        Ok(DeliveryReport {
            topic: retry_topic.to_owned(),
            partition,
            offset,
        })
    }
}

/// Extend `build_headers` output with an optional `x-rb-process-after-ms` header.
#[allow(clippy::used_underscore_binding)]
fn build_headers_with_retry<E: ProstMessage>(
    envelope: &EventEnvelope<E>,
    due_ms: Option<u64>,
) -> OwnedHeaders {
    let mut h = build_headers(envelope);
    if let Some(ms) = due_ms {
        h = h.insert(Header {
            key: HEADER_PROCESS_AFTER_MS,
            value: Some(ms.to_string().as_bytes()),
        });
    }
    h
}

#[allow(clippy::used_underscore_binding)]
fn build_headers<E: ProstMessage>(envelope: &EventEnvelope<E>) -> OwnedHeaders {
    let tenant_str = envelope.tenant_id.to_string();
    let event_id_str = envelope.event_id.to_string();
    let attempt_str = envelope._meta.attempt.to_string();
    let created_at_ms_str = envelope.created_at.timestamp_millis().to_string();

    let mut h = OwnedHeaders::new();
    h = h.insert(Header {
        key: HEADER_TENANT_ID,
        value: Some(tenant_str.as_bytes()),
    });
    h = h.insert(Header {
        key: HEADER_EVENT_ID,
        value: Some(event_id_str.as_bytes()),
    });
    h = h.insert(Header {
        key: HEADER_SCHEMA_VERSION,
        value: Some(envelope.schema_version.as_str().as_bytes()),
    });
    h = h.insert(Header {
        key: HEADER_ATTEMPT,
        value: Some(attempt_str.as_bytes()),
    });
    h = h.insert(Header {
        key: HEADER_CREATED_AT_MS,
        value: Some(created_at_ms_str.as_bytes()),
    });

    if let Some(ref blob_ref) = envelope.blob_ref {
        h = h.insert(Header {
            key: HEADER_BLOB_REF,
            value: Some(blob_ref.as_bytes()),
        });
    }

    // Explicit trace_context wins over the active OTel span: it carries an upstream
    // traceparent that a consumer wants to forward, so we write it verbatim and skip
    // the OTel inject to avoid a duplicate traceparent header (the first header wins
    // on decode via iter().find()).  When no explicit context is set, OTel inject
    // captures the current producer span (no-op if no propagator is installed).
    if let Some(ref tc) = envelope.trace_context {
        if !tc.traceparent.is_empty() {
            h = h.insert(Header {
                key: HEADER_TRACEPARENT,
                value: Some(tc.traceparent.as_bytes()),
            });
        }
        if !tc.tracestate.is_empty() {
            h = h.insert(Header {
                key: HEADER_TRACESTATE,
                value: Some(tc.tracestate.as_bytes()),
            });
        }
        h
    } else {
        // Explicitly extract the OTel context from the current tracing span rather than
        // relying on Context::current() — the tracing in_scope() bridge does not
        // populate the ambient OTel context reliably across all executor configurations.
        let cx = tracing::Span::current().context();
        let mut injector = KafkaHeaderInjector(h);
        opentelemetry::global::get_text_map_propagator(|prop| {
            prop.inject_context(&cx, &mut injector);
        });
        injector.0
    }
}

/// Decode an [`EventEnvelope<E>`] from raw Kafka message bytes + flattened headers.
pub fn decode_envelope<E: ProstMessage + Default>(
    payload: &[u8],
    headers: &[(String, String)],
    topic: &str,
    partition: i32,
    offset: i64,
) -> Result<EventEnvelope<E>, KafkaError> {
    let get = |key: &'static str| -> Option<String> {
        headers
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    let tenant_str = get(HEADER_TENANT_ID).ok_or(KafkaError::MissingHeader(HEADER_TENANT_ID))?;
    let tenant_id =
        tenant_str
            .parse::<rb_schemas::TenantId>()
            .map_err(|e| KafkaError::InvalidHeaderUuid {
                header: HEADER_TENANT_ID,
                source: e,
            })?;

    let event_id_str = get(HEADER_EVENT_ID).ok_or(KafkaError::MissingHeader(HEADER_EVENT_ID))?;
    let event_id =
        event_id_str
            .parse::<uuid::Uuid>()
            .map_err(|e| KafkaError::InvalidHeaderUuid {
                header: HEADER_EVENT_ID,
                source: e,
            })?;

    let schema_str =
        get(HEADER_SCHEMA_VERSION).ok_or(KafkaError::MissingHeader(HEADER_SCHEMA_VERSION))?;
    let schema_version =
        SchemaVersion::from_str(&schema_str).map_err(|e| KafkaError::SchemaMismatch {
            expected: SchemaVersion::V1.as_str().to_owned(),
            got: e,
        })?;

    let blob_ref = get(HEADER_BLOB_REF);
    let traceparent = get(HEADER_TRACEPARENT).unwrap_or_default();
    let tracestate = get(HEADER_TRACESTATE).unwrap_or_default();
    let trace_context = if traceparent.is_empty() {
        None
    } else {
        Some(TraceContext {
            traceparent,
            tracestate,
        })
    };

    let attempt = get(HEADER_ATTEMPT)
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    // Check process_after gate for retry backoff.
    if let Some(due_ms) = get(HEADER_PROCESS_AFTER_MS).and_then(|s| s.parse::<u64>().ok()) {
        #[allow(clippy::cast_possible_truncation)]
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        if now_ms < due_ms {
            return Err(KafkaError::Broker(
                "message not yet eligible (process_after_ms)".to_owned(),
            ));
        }
    }

    let payload_msg = E::decode(payload).map_err(|e| KafkaError::Deserialization(e.to_string()))?;

    let created_at = get(HEADER_CREATED_AT_MS)
        .and_then(|s| s.parse::<i64>().ok())
        .and_then(chrono::DateTime::from_timestamp_millis)
        .unwrap_or_else(chrono::Utc::now);

    Ok(EventEnvelope {
        tenant_id,
        event_id,
        schema_version,
        trace_context,
        blob_ref,
        created_at,
        payload: payload_msg,
        _meta: EnvelopeMeta {
            topic: topic.to_owned(),
            partition,
            offset,
            attempt,
        },
    })
}
