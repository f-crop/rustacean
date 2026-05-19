use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use metrics::counter;
use prost::Message as ProstMessage;
use rb_kafka::{ConsumerCfg, EventEnvelope, KafkaError};
use tracing::{error, info, warn};

use crate::config::WatchdogConfig;

// ---------------------------------------------------------------------------
// Internal consumer abstraction (enables mocking in tests)
// ---------------------------------------------------------------------------

/// Internal trait that abstracts over `rb_kafka::Consumer<E>` for testability.
/// Only `pub(crate)` — callers interact with `HealthyConsumer` directly.
#[async_trait]
pub(crate) trait ConsumerOps<E>: Send + Sync
where
    E: ProstMessage + Default + Send + Sync + 'static,
{
    async fn next(&self) -> Option<Result<EventEnvelope<E>, KafkaError>>;
    async fn commit(&self, env: &EventEnvelope<E>) -> Result<(), KafkaError>;
    async fn nack_to_dlq(&self, env: &EventEnvelope<E>, reason: &str) -> Result<(), KafkaError>;
    async fn lag_estimate(&self) -> u64;
    /// Number of topic-partition assignments held by this consumer.
    /// Returns `0` when the consumer-group membership has been lost.
    async fn assigned_partition_count(&self) -> usize;
}

#[async_trait]
impl<E> ConsumerOps<E> for rb_kafka::Consumer<E>
where
    E: ProstMessage + Default + Send + Sync + 'static,
{
    async fn next(&self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        rb_kafka::Consumer::next(self).await
    }

    async fn commit(&self, env: &EventEnvelope<E>) -> Result<(), KafkaError> {
        rb_kafka::Consumer::commit(self, env).await
    }

    async fn nack_to_dlq(&self, env: &EventEnvelope<E>, reason: &str) -> Result<(), KafkaError> {
        rb_kafka::Consumer::nack_to_dlq(self, env, reason).await
    }

    async fn lag_estimate(&self) -> u64 {
        self.assignment_lag_estimate().await
    }

    async fn assigned_partition_count(&self) -> usize {
        rb_kafka::Consumer::assignment_count(self).await
    }
}

// ---------------------------------------------------------------------------
// ConsumerFactory — injectable so tests can supply mock consumers
// ---------------------------------------------------------------------------

/// Creates a new `ConsumerOps<E>` instance, subscribing it to `topics`.
/// Used by `HealthyConsumer` when it needs to recreate after a wedge.
type ConsumerFactory<E> =
    Arc<dyn Fn() -> Result<Box<dyn ConsumerOps<E>>, KafkaError> + Send + Sync>;

// ---------------------------------------------------------------------------
// KafkaHealthWatcher — factory
// ---------------------------------------------------------------------------

/// Wraps a `Consumer<E>` with a watchdog that detects wedge states and
/// transparently recreates the consumer when stalled.
///
/// Detected conditions:
///
/// 1. **Lag wedge** — lag > 0 but no messages for `stall_timeout`.
/// 2. **Group membership loss** — consumer previously received messages but now
///    has zero assigned partitions (group evaporated after broker restart).
///    Detected when lag == 0 on stall and `assigned_partition_count == 0`.
/// 3. **Error cascade** — consecutive errors from `next()` exceed
///    `max_error_streak`; catches the silent reconnect-retry loop
///    (`REQTMOUT` / `ApiVersionRequest failed: Local: Timed out`) that
///    produces repeated stream errors without progress.
pub struct KafkaHealthWatcher;

impl KafkaHealthWatcher {
    /// Wrap `consumer` with the watchdog.
    ///
    /// * `cfg` — the same `ConsumerCfg` used to create `consumer`; reused
    ///   when the watchdog recreates the consumer after a wedge.
    /// * `topics` — topic list passed to `subscribe()`; reused on recreate.
    /// * `watchdog_cfg` — stall timeout and error-streak settings.
    #[must_use]
    pub fn wrap<E>(
        consumer: rb_kafka::Consumer<E>,
        cfg: &ConsumerCfg,
        topics: &[String],
        watchdog_cfg: WatchdogConfig,
    ) -> HealthyConsumer<E>
    where
        E: ProstMessage + Default + Send + Sync + 'static,
    {
        let cfg2 = cfg.clone();
        let topics2 = topics.to_owned();
        let factory: ConsumerFactory<E> = Arc::new(move || {
            let c = rb_kafka::Consumer::<E>::new(&cfg2)?;
            let refs: Vec<&str> = topics2.iter().map(String::as_str).collect();
            c.subscribe(&refs)?;
            Ok(Box::new(c))
        });

        HealthyConsumer::with_factory(Box::new(consumer), factory, watchdog_cfg)
    }
}

// ---------------------------------------------------------------------------
// HealthyConsumer — the wrapper returned to callers
// ---------------------------------------------------------------------------

/// A `Consumer<E>` wrapper that automatically recreates the consumer when a
/// wedge state is detected.
///
/// Drop-in for `rb_kafka::Consumer<E>` at call sites that only use `next()`
/// and `commit()`.
pub struct HealthyConsumer<E>
where
    E: ProstMessage + Default + Send + Sync + 'static,
{
    inner: Box<dyn ConsumerOps<E>>,
    factory: ConsumerFactory<E>,
    config: WatchdogConfig,
    last_recv: Instant,
    /// How many times the inner consumer has been recreated.
    pub recreate_count: u64,
    /// `true` once the consumer has successfully delivered at least one message.
    /// Guards group-loss detection so startup races don't cause spurious restarts.
    pub(crate) had_assignment: bool,
    /// Consecutive error results from `next()` without an intervening success.
    pub(crate) consecutive_errors: u32,
}

impl<E> HealthyConsumer<E>
where
    E: ProstMessage + Default + Send + Sync + 'static,
{
    pub(crate) fn with_factory(
        inner: Box<dyn ConsumerOps<E>>,
        factory: ConsumerFactory<E>,
        config: WatchdogConfig,
    ) -> Self {
        Self {
            inner,
            factory,
            config,
            last_recv: Instant::now(),
            recreate_count: 0,
            had_assignment: false,
            consecutive_errors: 0,
        }
    }

    /// Recreate the inner consumer via the factory.  Resets the stall clock
    /// and `had_assignment` on success so the watchdog starts fresh.
    async fn do_recreate(&mut self) {
        match (self.factory)() {
            Ok(new_inner) => {
                self.inner = new_inner;
                self.recreate_count += 1;
                self.last_recv = Instant::now();
                self.had_assignment = false;
                self.consecutive_errors = 0;
                info!(
                    recreate_count = self.recreate_count,
                    "kafka consumer recreated successfully"
                );
            }
            Err(e) => {
                error!(error = %e, "kafka consumer recreation failed; will retry");
                // Brief back-off; last_recv intentionally NOT reset so the stall
                // timer fires again immediately, driving retry.
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }

    /// Receive the next message, transparently recreating the consumer on any
    /// detected wedge state.
    ///
    /// Mirrors the `Option<Result<…>>` signature of `rb_kafka::Consumer::next`.
    pub async fn next(&mut self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        loop {
            let elapsed = self.last_recv.elapsed();
            let remaining = self.config.stall_timeout.checked_sub(elapsed);

            // ----------------------------------------------------------------
            // Fast path: stall timeout hasn't expired — poll the consumer.
            // ----------------------------------------------------------------
            match remaining {
                Some(d) if d > Duration::ZERO => {
                    match tokio::time::timeout(d, self.inner.next()).await {
                        Ok(inner_result) => {
                            // Inspect before returning so we can update watchdog
                            // state without consuming the value.
                            let is_ok = matches!(inner_result, Some(Ok(_)));

                            if is_ok {
                                self.last_recv = Instant::now();
                                self.consecutive_errors = 0;
                                self.had_assignment = true;
                            } else if matches!(inner_result, Some(Err(_))) {
                                self.consecutive_errors = self.consecutive_errors.saturating_add(1);
                                if self.config.max_error_streak > 0
                                    && self.consecutive_errors >= self.config.max_error_streak
                                {
                                    warn!(
                                        consecutive = self.consecutive_errors,
                                        threshold = self.config.max_error_streak,
                                        "kafka error cascade detected; recreating consumer"
                                    );
                                    counter!("rb_kafka_health_error_streak_restart_total")
                                        .increment(1);
                                    self.do_recreate().await;
                                    continue;
                                }
                            }

                            return inner_result;
                        }
                        Err(_elapsed) => {
                            // Stall timeout expired — fall through to stall handling.
                        }
                    }
                }
                _ => {
                    // remaining is None or zero — already in stall state.
                }
            }

            // ----------------------------------------------------------------
            // Stall path: no successful message for stall_timeout.
            // ----------------------------------------------------------------
            let lag = self.inner.lag_estimate().await;

            if lag > 0 {
                // Classic wedge: lag present but consumer is frozen.
                warn!(
                    lag,
                    stall_secs = self.config.stall_timeout.as_secs(),
                    "kafka consumer wedge detected; recreating"
                );
                counter!("rb_kafka_health_wedge_total").increment(1);
                self.do_recreate().await;
            } else if self.had_assignment {
                // Lag is zero but we previously received messages.
                // Check whether the consumer-group membership was lost —
                // after a broker restart the group can evaporate, leaving the
                // consumer with zero partitions.  lag_estimate() returns 0 in
                // this state because there is no assignment to compute lag from,
                // masking the wedge from the lag-based check above.
                let partitions = self.inner.assigned_partition_count().await;
                if partitions == 0 {
                    warn!(
                        stall_secs = self.config.stall_timeout.as_secs(),
                        "consumer group membership lost (0 partitions); recreating"
                    );
                    counter!("rb_kafka_health_group_loss_restart_total").increment(1);
                    self.do_recreate().await;
                } else {
                    // Genuinely quiet topic with valid assignment; reset clock.
                    self.last_recv = Instant::now();
                }
            } else {
                // No messages received yet (startup or quiet topic); reset clock.
                self.last_recv = Instant::now();
            }
        }
    }

    /// Commit an offset via the inner consumer.
    pub async fn commit(&self, env: &EventEnvelope<E>) -> Result<(), KafkaError> {
        self.inner.commit(env).await
    }

    /// Route a message to the dead-letter queue via the inner consumer.
    pub async fn nack_to_dlq(
        &self,
        env: &EventEnvelope<E>,
        reason: &str,
    ) -> Result<(), KafkaError> {
        self.inner.nack_to_dlq(env, reason).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "watcher_tests.rs"]
mod tests;
