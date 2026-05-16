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
    async fn lag_estimate(&self) -> u64;
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

    async fn lag_estimate(&self) -> u64 {
        self.assignment_lag_estimate().await
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

/// Wraps a `Consumer<E>` with a watchdog that detects the
/// `wait-unassign-to-complete` wedge state and transparently recreates the
/// consumer when stalled.
pub struct KafkaHealthWatcher;

impl KafkaHealthWatcher {
    /// Wrap `consumer` with the watchdog.
    ///
    /// * `cfg` — the same `ConsumerCfg` used to create `consumer`; reused
    ///   when the watchdog recreates the consumer after a wedge.
    /// * `topics` — topic list passed to `subscribe()`; reused on recreate.
    /// * `watchdog_cfg` — stall timeout settings.
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
/// wedge state is detected (no messages for `stall_timeout` while lag > 0).
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
        }
    }

    /// Receive the next message, transparently recreating the consumer if a
    /// wedge state is detected.
    ///
    /// Mirrors the `Option<Result<…>>` signature of `rb_kafka::Consumer::next`.
    pub async fn next(&mut self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        loop {
            let elapsed = self.last_recv.elapsed();
            let remaining = self.config.stall_timeout.checked_sub(elapsed);

            let timed_out = match remaining {
                None | Some(Duration::ZERO) => true,
                Some(d) => match tokio::time::timeout(d, self.inner.next()).await {
                    Ok(result) => {
                        self.last_recv = Instant::now();
                        return result;
                    }
                    Err(_timeout) => true,
                },
            };

            if timed_out {
                // Check whether there are actually messages to consume.
                let lag = self.inner.lag_estimate().await;
                if lag == 0 {
                    // Topic is genuinely quiet; reset stall clock and loop.
                    self.last_recv = Instant::now();
                    continue;
                }

                // Wedge confirmed: lag > 0 but nothing received for stall_timeout.
                warn!(
                    lag,
                    stall_secs = self.config.stall_timeout.as_secs(),
                    "kafka consumer wedge detected; recreating"
                );
                counter!("rb_kafka_health_wedge_total").increment(1);

                match (self.factory)() {
                    Ok(new_inner) => {
                        self.inner = new_inner;
                        self.recreate_count += 1;
                        self.last_recv = Instant::now();
                        info!(
                            recreate_count = self.recreate_count,
                            "kafka consumer recreated successfully"
                        );
                    }
                    Err(e) => {
                        error!(error = %e, "kafka consumer recreation failed; will retry");
                        // Brief back-off before the next attempt.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    /// Commit an offset via the inner consumer.
    pub async fn commit(&self, env: &EventEnvelope<E>) -> Result<(), KafkaError> {
        self.inner.commit(env).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_kafka::ConsumerCfg;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::sync::Mutex;

    // ── Mock consumer ────────────────────────────────────────────────────────

    type MsgQueue<E> = Mutex<Vec<Option<Result<EventEnvelope<E>, KafkaError>>>>;

    /// A consumer that either returns prepared messages or stalls indefinitely.
    struct MockConsumer<E: ProstMessage + Default + Send + Sync + 'static> {
        #[allow(dead_code)]
        messages: MsgQueue<E>,
        lag: Arc<AtomicU64>,
    }

    impl<E: ProstMessage + Default + Send + Sync + 'static> MockConsumer<E> {
        fn stalling(lag: Arc<AtomicU64>) -> Self {
            Self {
                messages: Mutex::new(vec![]),
                lag,
            }
        }
    }

    #[async_trait]
    impl<E: ProstMessage + Default + Send + Sync + 'static> ConsumerOps<E> for MockConsumer<E> {
        async fn next(&self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
            let mut guard = self.messages.lock().await;
            if guard.is_empty() {
                drop(guard);
                // Block until the caller's timeout fires.
                std::future::pending::<()>().await;
                unreachable!()
            }
            guard.remove(0)
        }

        async fn commit(&self, _env: &EventEnvelope<E>) -> Result<(), KafkaError> {
            Ok(())
        }

        async fn lag_estimate(&self) -> u64 {
            self.lag.load(Ordering::Relaxed)
        }
    }

    #[allow(dead_code)]
    fn dummy_cfg() -> ConsumerCfg {
        ConsumerCfg::new("test-group")
    }

    // ── wedge detection: lag > 0 triggers recreate ───────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wedge_with_lag_triggers_recreate() {
        let recreate_counter = Arc::new(AtomicU64::new(0));
        let recreate_counter2 = Arc::clone(&recreate_counter);

        let lag = Arc::new(AtomicU64::new(100)); // non-zero: signals wedge
        let lag2 = Arc::clone(&lag);

        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            recreate_counter2.fetch_add(1, Ordering::Relaxed);
            let lag3 = Arc::clone(&lag2);
            Ok(Box::new(MockConsumer::stalling(lag3))
                as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
        });

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(
                Arc::clone(&lag),
            )),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(50),
            },
        );

        // After two stall cycles the watchdog should have recreated at least once.
        // We give it enough time for 2× stall_timeout + recreation + one more cycle.
        tokio::time::timeout(Duration::from_millis(400), healthy.next())
            .await
            .ok();

        assert!(
            recreate_counter.load(Ordering::Relaxed) >= 1,
            "watchdog should have called the factory at least once"
        );
        assert!(
            healthy.recreate_count >= 1,
            "recreate_count should be >= 1 after a wedge"
        );
    }

    // ── no recreate when lag is zero ─────────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_recreate_when_lag_is_zero() {
        let recreate_calls = Arc::new(AtomicU64::new(0));
        let recreate_calls2 = Arc::clone(&recreate_calls);

        let lag = Arc::new(AtomicU64::new(0)); // zero lag: topic is quiet

        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            recreate_calls2.fetch_add(1, Ordering::Relaxed);
            Err(KafkaError::Broker("should not be called".into()))
        });

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(
                lag,
            )),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(30),
            },
        );

        tokio::time::timeout(Duration::from_millis(200), healthy.next())
            .await
            .ok();

        assert_eq!(
            recreate_calls.load(Ordering::Relaxed),
            0,
            "factory must not be called when lag is zero"
        );
        assert_eq!(
            healthy.recreate_count, 0,
            "recreate_count must stay zero when lag is zero"
        );
    }

    // ── recreate_count increments for each successful recreation ─────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recreate_count_tracks_multiple_recreations() {
        let lag = Arc::new(AtomicU64::new(50));

        // Factory always succeeds and returns a stalling consumer.
        let lag2 = Arc::clone(&lag);
        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            let l = Arc::clone(&lag2);
            Ok(
                Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(l))
                    as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>,
            )
        });

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(
                Arc::clone(&lag),
            )),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(30),
            },
        );

        // Run for 5× the stall_timeout so the watchdog fires multiple times.
        tokio::time::timeout(Duration::from_millis(250), healthy.next())
            .await
            .ok();

        assert!(
            healthy.recreate_count >= 2,
            "expected at least 2 recreations in 5× stall window, got {}",
            healthy.recreate_count
        );
    }
}
