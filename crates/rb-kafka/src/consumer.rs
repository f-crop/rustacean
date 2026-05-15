use std::{marker::PhantomData, sync::Arc, time::Duration};

use futures::StreamExt as _;
use metrics::{counter, gauge, histogram};
use prost::Message as ProstMessage;
use rdkafka::{
    ClientConfig, TopicPartitionList,
    consumer::{CommitMode, Consumer as RdConsumer, StreamConsumer},
    message::{Header, Headers as RdHeaders, Message as RdMessage},
};

use crate::{
    config::ConsumerCfg,
    dlq::dlq_topic,
    envelope::{
        EventEnvelope, HEADER_ATTEMPT, HEADER_BLOB_REF, HEADER_CREATED_AT_MS, HEADER_DLQ_AT,
        HEADER_DLQ_REASON, HEADER_EVENT_ID, HEADER_SCHEMA_VERSION, HEADER_TENANT_ID,
        HEADER_TRACEPARENT, HEADER_TRACESTATE,
    },
    errors::KafkaError,
    headers::{KafkaHeaderExtractor, is_valid_traceparent},
    producer::decode_envelope,
};

pub struct Consumer<E: ProstMessage + Default> {
    inner: Arc<StreamConsumer>,
    dlq_producer: Arc<rdkafka::producer::FutureProducer>,
    _phantom: PhantomData<E>,
}

#[allow(clippy::used_underscore_binding, clippy::unused_async)]
impl<E: ProstMessage + Default> Consumer<E> {
    pub fn new(cfg: &ConsumerCfg) -> Result<Self, KafkaError> {
        let mut client_cfg = ClientConfig::new();
        client_cfg
            .set("bootstrap.servers", &cfg.bootstrap_servers)
            .set("group.id", &cfg.group_id)
            .set("enable.auto.commit", cfg.enable_auto_commit.to_string())
            .set("isolation.level", &cfg.isolation_level)
            .set("auto.offset.reset", &cfg.auto_offset_reset)
            .set(
                "max.poll.interval.ms",
                cfg.max_poll_interval.as_millis().to_string(),
            )
            .set(
                "session.timeout.ms",
                cfg.session_timeout.as_millis().to_string(),
            )
            .set(
                "heartbeat.interval.ms",
                cfg.heartbeat_interval.as_millis().to_string(),
            );
        if let Some(ref iid) = cfg.instance_id {
            client_cfg.set("group.instance.id", iid);
        }
        let consumer: StreamConsumer = client_cfg
            // Dev brokers can take >100ms for first-fetch offset requests;
            // 30 s matches librdkafka's protocol default and silences REQTMOUT noise.
            .set("socket.timeout.ms", "30000")
            .set("request.timeout.ms", "30000")
            // librdkafka caches "topic does not exist" responses for the full
            // refresh interval (default 5 min). When the broker creates the
            // topic just after our first metadata fetch — e.g., compose
            // kafka-init -> consumer race — the stream goes silent for 5 min.
            // 10 s gives us quick recovery without saturating the broker.
            .set("topic.metadata.refresh.interval.ms", "10000")
            .create()?;

        // DLQ delivery is best-effort: acks=1 avoids blocking the consume loop on
        // broker unavailability. Losing a DLQ record is preferable to stalling the
        // pipeline; ops alerts on rb_kafka_dlq_total drops catch silent failures.
        let dlq_producer = ClientConfig::new()
            .set("bootstrap.servers", &cfg.bootstrap_servers)
            .set("acks", "1")
            .create()?;

        Ok(Self {
            inner: Arc::new(consumer),
            dlq_producer: Arc::new(dlq_producer),
            _phantom: PhantomData,
        })
    }

    pub fn subscribe(&self, topics: &[&str]) -> Result<(), KafkaError> {
        self.inner.subscribe(topics)?;
        Ok(())
    }

    /// Block until every named topic is visible in broker metadata, or `timeout` elapses.
    ///
    /// Defense against the kafka-init -> consumer race in docker compose: even when
    /// `depends_on: kafka-init: service_completed_successfully` orders the start,
    /// the first `subscribe()` can race a still-propagating metadata refresh and the
    /// consumer logs `UnknownTopicOrPartition` once, then goes silent for up to one
    /// `topic.metadata.refresh.interval.ms` window. Calling this *before* subscribe
    /// surfaces the missing topic as a loud error and turns it into a bounded retry.
    ///
    /// Per attempt we ask the broker for metadata with a 5 s timeout, sleep 1 s
    /// between attempts on failure, and use `spawn_blocking` so the rdkafka
    /// blocking call does not stall the tokio reactor.
    pub async fn wait_for_topics(
        &self,
        topics: &[&str],
        timeout: Duration,
    ) -> Result<(), KafkaError> {
        let deadline = std::time::Instant::now() + timeout;
        let inner = self.inner.clone();
        let topic_set: Vec<String> = topics.iter().map(|t| (*t).to_owned()).collect();
        let mut attempt: u32 = 0;

        loop {
            attempt += 1;
            let inner = inner.clone();
            let topic_set_clone = topic_set.clone();
            let metadata_result = tokio::task::spawn_blocking(move || {
                inner
                    .fetch_metadata(None, Duration::from_secs(5))
                    .map(|md| {
                        topic_set_clone
                            .iter()
                            .map(|wanted| {
                                md.topics()
                                    .iter()
                                    .find(|t| t.name() == wanted.as_str())
                                    .map(|t| {
                                        (
                                            wanted.clone(),
                                            !t.partitions().is_empty() && t.error().is_none(),
                                        )
                                    })
                                    .unwrap_or((wanted.clone(), false))
                            })
                            .collect::<Vec<(String, bool)>>()
                    })
            })
            .await
            .map_err(|join_err| KafkaError::Broker(format!("join error: {join_err}")))?;

            match metadata_result {
                Ok(status) => {
                    let missing: Vec<String> = status
                        .iter()
                        .filter(|(_, ok)| !ok)
                        .map(|(name, _)| name.clone())
                        .collect();
                    if missing.is_empty() {
                        tracing::info!(?topic_set, attempt, "kafka topics visible");
                        return Ok(());
                    }
                    tracing::warn!(?missing, attempt, "kafka topics not visible yet; retrying");
                }
                Err(e) => {
                    tracing::warn!(error = %e, attempt, "fetch_metadata failed; retrying");
                }
            }

            if std::time::Instant::now() >= deadline {
                return Err(KafkaError::Broker(format!(
                    "topics {topic_set:?} not visible after {attempt} attempts within {timeout:?}"
                )));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    #[allow(clippy::too_many_lines)]
    pub async fn next(&self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        let msg = self.inner.stream().next().await?;
        match msg {
            Err(e) => Some(Err(KafkaError::from(e))),
            Ok(m) => {
                let payload = m.payload().unwrap_or(&[]);
                let headers = extract_headers(m.headers());
                let topic = m.topic().to_owned();
                let partition = m.partition();
                let offset = m.offset();

                // Emit consumer lag gauge if watermark info is available.
                // rdkafka StreamConsumer exposes fetch_watermarks synchronously.
                if let Ok((_, high)) =
                    self.inner
                        .fetch_watermarks(&topic, partition, Duration::from_millis(500))
                {
                    #[allow(clippy::cast_precision_loss)]
                    let lag = (high - offset).max(0) as f64;
                    gauge!(
                        "rb_kafka_consumer_lag_records",
                        "topic" => topic.clone(),
                        "partition" => partition.to_string(),
                        "group" => String::new()
                    )
                    .set(lag);
                }

                // Build consume span as child of the upstream producer trace.
                // _cx_guard is scoped to span construction: the span captures the parent
                // relationship at creation time; both _cx_guard and key_str are dropped
                // before any .await point.
                let consume_span = {
                    let extractor = KafkaHeaderExtractor(&headers);
                    let _cx_guard = opentelemetry::global::get_text_map_propagator(|prop| {
                        prop.extract(&extractor)
                    })
                    .attach();
                    let key_str = m
                        .key()
                        .map(|k| String::from_utf8_lossy(k).into_owned())
                        .unwrap_or_default();
                    tracing::info_span!(
                        "kafka.consume",
                        "otel.kind" = "CONSUMER",
                        "messaging.system" = "kafka",
                        "messaging.destination" = %topic,
                        "messaging.kafka.partition" = partition,
                        "messaging.kafka.offset" = offset,
                        "messaging.kafka.message_key" = %key_str,
                        "rb.tenant_id" = tracing::field::Empty,
                        "rb.event_id" = tracing::field::Empty,
                        "rb.schema_version" = tracing::field::Empty,
                        "rb.attempt" = tracing::field::Empty,
                    )
                    // key_str and _cx_guard dropped here
                };
                // Scoped entry: span active only for the synchronous decode; guard
                // drops at block exit so it is never held across an .await point.
                let result = {
                    let _enter = consume_span.enter();
                    decode_envelope(payload, &headers, &topic, partition, offset)
                };

                // Record §9.3 attributes immediately after decode so all exit paths,
                // including the malformed-traceparent early return, carry envelope
                // attributes on the span.  span.record() works without the span entered.
                if let Ok(ref env) = result {
                    consume_span.record("rb.tenant_id", env.tenant_id.to_string().as_str());
                    consume_span.record("rb.event_id", env.event_id.to_string().as_str());
                    consume_span.record("rb.schema_version", env.schema_version.as_str());
                    consume_span.record("rb.attempt", env._meta.attempt);
                }

                // Malformed traceparent: DLQ immediately and surface the error.
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
                            // Best-effort: losing a DLQ record is preferable to stalling;
                            // rb_kafka_dlq_total drops are caught by ops alerts.
                            let _ = self.nack_to_dlq(&env, "invalid-traceparent").await;
                            let _ = self.commit(&env).await;
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
                        // Measure wall-clock lag from when the envelope was created.
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
        }
    }

    pub async fn commit(&self, env: &EventEnvelope<E>) -> Result<(), KafkaError> {
        let mut tpl = TopicPartitionList::new();
        tpl.add_partition_offset(
            &env._meta.topic,
            env._meta.partition,
            rdkafka::Offset::Offset(env._meta.offset + 1),
        )?;
        self.inner.commit(&tpl, CommitMode::Async)?;
        Ok(())
    }

    pub async fn nack_to_dlq(
        &self,
        env: &EventEnvelope<E>,
        reason: &str,
    ) -> Result<(), KafkaError> {
        use rdkafka::producer::FutureRecord;

        let dlq = dlq_topic(&env._meta.topic);
        let dlq_at = chrono::Utc::now().timestamp_millis().to_string();

        // Re-build all ADR-006 §3.1 envelope headers so decode_envelope succeeds on DLQ.
        let tenant_str = env.tenant_id.to_string();
        let event_id_str = env.event_id.to_string();
        let attempt_str = env._meta.attempt.to_string();
        let schema_str = env.schema_version.as_str();
        let created_at_ms_str = env.created_at.timestamp_millis().to_string();

        let mut headers = rdkafka::message::OwnedHeaders::new()
            .insert(Header {
                key: HEADER_TENANT_ID,
                value: Some(tenant_str.as_bytes()),
            })
            .insert(Header {
                key: HEADER_EVENT_ID,
                value: Some(event_id_str.as_bytes()),
            })
            .insert(Header {
                key: HEADER_SCHEMA_VERSION,
                value: Some(schema_str.as_bytes()),
            })
            .insert(Header {
                key: HEADER_ATTEMPT,
                value: Some(attempt_str.as_bytes()),
            })
            .insert(Header {
                key: HEADER_CREATED_AT_MS,
                value: Some(created_at_ms_str.as_bytes()),
            });

        if let Some(ref blob_ref) = env.blob_ref {
            headers = headers.insert(Header {
                key: HEADER_BLOB_REF,
                value: Some(blob_ref.as_bytes()),
            });
        }
        if let Some(ref tc) = env.trace_context {
            if !tc.traceparent.is_empty() {
                headers = headers.insert(Header {
                    key: HEADER_TRACEPARENT,
                    value: Some(tc.traceparent.as_bytes()),
                });
            }
            if !tc.tracestate.is_empty() {
                headers = headers.insert(Header {
                    key: HEADER_TRACESTATE,
                    value: Some(tc.tracestate.as_bytes()),
                });
            }
        }
        headers = headers
            .insert(Header {
                key: HEADER_DLQ_REASON,
                value: Some(reason.as_bytes()),
            })
            .insert(Header {
                key: HEADER_DLQ_AT,
                value: Some(dlq_at.as_bytes()),
            });

        let payload = env.payload.encode_to_vec();
        let key = env.tenant_id.to_string();
        let record = FutureRecord::to(dlq.as_str())
            .key(key.as_bytes())
            .payload(payload.as_slice())
            .headers(headers);

        self.dlq_producer
            .send(record, Duration::from_secs(10))
            .await
            .map_err(|(e, _)| KafkaError::from(e))?;

        counter!(
            "rb_kafka_dlq_total",
            "topic" => env._meta.topic.clone(),
            "reason" => reason.to_owned()
        )
        .increment(1);

        Ok(())
    }
}

fn extract_headers(h: Option<&rdkafka::message::BorrowedHeaders>) -> Vec<(String, String)> {
    let Some(bh) = h else { return Vec::new() };
    bh.iter()
        .filter_map(|header| {
            let key = header.key.to_owned();
            let value = std::str::from_utf8(header.value.unwrap_or(&[]))
                .ok()?
                .to_owned();
            Some((key, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::AgentSessionCommand;

    /// Defense for the agent-runner kafka startup race (RUSAA-1171):
    /// pointing the consumer at an unreachable bootstrap address must
    /// time out the `wait_for_topics()` helper instead of hanging forever
    /// or sliding past silently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wait_for_topics_times_out_when_broker_unreachable() {
        let cfg = ConsumerCfg {
            bootstrap_servers: "127.0.0.1:1".to_owned(), // no kafka here
            group_id: "rb-kafka-wait-for-topics-test".to_owned(),
            instance_id: None,
            enable_auto_commit: false,
            isolation_level: "read_committed".to_owned(),
            auto_offset_reset: "earliest".to_owned(),
            max_poll_interval: Duration::from_secs(600),
            session_timeout: Duration::from_secs(60),
            heartbeat_interval: Duration::from_secs(10),
        };
        let consumer: Consumer<AgentSessionCommand> =
            Consumer::new(&cfg).expect("consumer construction");
        let err = consumer
            .wait_for_topics(&["rb.agent.commands"], Duration::from_millis(1500))
            .await
            .expect_err("expected timeout error against unreachable broker");
        let msg = format!("{err}");
        assert!(
            msg.contains("rb.agent.commands"),
            "error message should name the missing topic: {msg}"
        );
    }
}
