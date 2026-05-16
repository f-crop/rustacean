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
/// transparently recreates the consumer.
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
/// wedge state is detected.  Two independent detection paths:
///
/// 1. **Stall path**: no *successful* message for `stall_timeout` while lag > 0,
///    or lag == 0 but the consumer has been returning errors (group may be gone).
/// 2. **Error-streak path**: `max_error_streak` consecutive errors force an
///    immediate recreate regardless of lag — catches the "consumer group
///    disappeared while binary is up" scenario where `lag_estimate()` returns 0.
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
    /// Timestamp of the last *successfully received* message (not errors).
    last_ok: Instant,
    /// Number of consecutive `Err` returns from `next()` without an intervening
    /// success.  Reset to 0 on each successful message or recreation.
    consecutive_errors: u32,
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
            last_ok: Instant::now(),
            consecutive_errors: 0,
            recreate_count: 0,
        }
    }

    /// Receive the next message, transparently recreating the consumer if a
    /// wedge state is detected.
    ///
    /// Mirrors the `Option<Result<…>>` signature of `rb_kafka::Consumer::next`.
    pub async fn next(&mut self) -> Option<Result<EventEnvelope<E>, KafkaError>> {
        loop {
            // Error-streak path: too many consecutive errors → force recreate
            // even if lag_estimate() returns 0 (group may have disappeared).
            if self.consecutive_errors >= self.config.max_error_streak {
                warn!(
                    streak = self.consecutive_errors,
                    "kafka consumer error streak exceeded; recreating"
                );
                counter!("rb_kafka_health_wedge_total").increment(1);
                self.do_recreate().await;
                continue;
            }

            let elapsed = self.last_ok.elapsed();
            let remaining = self.config.stall_timeout.checked_sub(elapsed);

            let timed_out = match remaining {
                None | Some(Duration::ZERO) => true,
                Some(d) => match tokio::time::timeout(d, self.inner.next()).await {
                    Ok(Some(Ok(envelope))) => {
                        // Successful message: reset both watchdog signals.
                        self.last_ok = Instant::now();
                        self.consecutive_errors = 0;
                        return Some(Ok(envelope));
                    }
                    Ok(Some(Err(e))) => {
                        // Kafka error: track streak but do NOT reset stall clock.
                        self.consecutive_errors += 1;
                        return Some(Err(e));
                    }
                    Ok(None) => {
                        return None;
                    }
                    Err(_timeout) => true,
                },
            };

            if timed_out {
                let lag = self.inner.lag_estimate().await;
                // Recreate if:
                //   - lag > 0 (messages pending but none received — classic wedge)
                //   - lag == 0 but we've been returning errors (group may be gone)
                if lag == 0 && self.consecutive_errors == 0 {
                    // Topic is genuinely quiet; reset stall clock and loop.
                    self.last_ok = Instant::now();
                    continue;
                }

                warn!(
                    lag,
                    consecutive_errors = self.consecutive_errors,
                    stall_secs = self.config.stall_timeout.as_secs(),
                    "kafka consumer wedge detected; recreating"
                );
                counter!("rb_kafka_health_wedge_total").increment(1);
                self.do_recreate().await;
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

    async fn do_recreate(&mut self) {
        match (self.factory)() {
            Ok(new_inner) => {
                self.inner = new_inner;
                self.recreate_count += 1;
                self.consecutive_errors = 0;
                self.last_ok = Instant::now();
                info!(
                    recreate_count = self.recreate_count,
                    "kafka consumer recreated successfully"
                );
            }
            Err(e) => {
                error!(error = %e, "kafka consumer recreation failed; will retry");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
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

        /// Returns errors immediately (simulates REQTMOUT reconnect loop).
        fn erroring(lag: Arc<AtomicU64>, error_count: usize) -> Self {
            let errors = (0..error_count)
                .map(|_| Some(Err(KafkaError::Broker("REQTMOUT".into()))))
                .collect();
            Self {
                messages: Mutex::new(errors),
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

        async fn nack_to_dlq(
            &self,
            _env: &EventEnvelope<E>,
            _reason: &str,
        ) -> Result<(), KafkaError> {
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
                max_error_streak: 100, // disable streak path for this test
            },
        );

        // After two stall cycles the watchdog should have recreated at least once.
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

    // ── no recreate when lag is zero and no errors ───────────────────────────

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
                max_error_streak: 100, // disable streak path for this test
            },
        );

        tokio::time::timeout(Duration::from_millis(200), healthy.next())
            .await
            .ok();

        assert_eq!(
            recreate_calls.load(Ordering::Relaxed),
            0,
            "factory must not be called when lag is zero and no errors"
        );
        assert_eq!(
            healthy.recreate_count, 0,
            "recreate_count must stay zero when lag is zero and no errors"
        );
    }

    // ── recreate_count increments for each successful recreation ─────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recreate_count_tracks_multiple_recreations() {
        let lag = Arc::new(AtomicU64::new(50));

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
                max_error_streak: 100, // disable streak path for this test
            },
        );

        tokio::time::timeout(Duration::from_millis(250), healthy.next())
            .await
            .ok();

        assert!(
            healthy.recreate_count >= 2,
            "expected at least 2 recreations in 5× stall window, got {}",
            healthy.recreate_count
        );
    }

    // ── error streak: recreate after N consecutive errors, even with lag==0 ──
    // This is the "consumer group disappeared" wedge from RUSAA-1517.

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn error_streak_triggers_recreate_when_lag_is_zero() {
        let recreate_counter = Arc::new(AtomicU64::new(0));
        let recreate_counter2 = Arc::clone(&recreate_counter);

        // lag == 0: stall path would not recreate on its own
        let lag = Arc::new(AtomicU64::new(0));
        let lag2 = Arc::clone(&lag);

        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            recreate_counter2.fetch_add(1, Ordering::Relaxed);
            // New consumer is also an erroring one (simulate persistent failure)
            let l = Arc::clone(&lag2);
            Ok(Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(l))
                as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
        });

        // 5 immediate REQTMOUT errors, max_error_streak=3 → recreate after 3rd
        let initial_consumer = MockConsumer::<rb_schemas::AgentSessionCommand>::erroring(
            Arc::clone(&lag),
            5,
        );

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(initial_consumer),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_secs(60), // long — stall path inactive
                max_error_streak: 3,
            },
        );

        // Drive next() until the errors are consumed and recreation fires.
        // Each error call returns immediately so this should be fast.
        for _ in 0..3 {
            let _ = healthy.next().await;
        }
        // 4th call should trigger recreation (streak == max_error_streak)
        tokio::time::timeout(Duration::from_millis(100), healthy.next())
            .await
            .ok();

        assert!(
            recreate_counter.load(Ordering::Relaxed) >= 1,
            "error streak should have triggered recreation"
        );
        assert!(
            healthy.recreate_count >= 1,
            "recreate_count should be >= 1 after error streak"
        );
    }

    // ── errors do not reset stall clock ──────────────────────────────────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn errors_do_not_reset_stall_clock() {
        let recreate_counter = Arc::new(AtomicU64::new(0));
        let recreate_counter2 = Arc::clone(&recreate_counter);

        // lag > 0 so the stall path will fire
        let lag = Arc::new(AtomicU64::new(10));
        let lag2 = Arc::clone(&lag);

        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            recreate_counter2.fetch_add(1, Ordering::Relaxed);
            let l = Arc::clone(&lag2);
            Ok(Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(l))
                as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
        });

        // Returns errors immediately then stalls — if errors wrongly reset the
        // stall clock, recreation would be delayed past the test timeout.
        let initial_consumer =
            MockConsumer::<rb_schemas::AgentSessionCommand>::erroring(Arc::clone(&lag), 2);

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(initial_consumer),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(50),
                max_error_streak: 100, // disable streak path
            },
        );

        // Consume the two errors (they should be returned to caller, not
        // absorbed by the watchdog).
        let r1 = healthy.next().await;
        let r2 = healthy.next().await;
        assert!(matches!(r1, Some(Err(_))), "first call should return error");
        assert!(matches!(r2, Some(Err(_))), "second call should return error");

        // Now the consumer stalls.  Because errors didn't reset the stall clock,
        // the stall_timeout should fire quickly and trigger recreation.
        tokio::time::timeout(Duration::from_millis(200), healthy.next())
            .await
            .ok();

        assert!(
            recreate_counter.load(Ordering::Relaxed) >= 1,
            "stall should fire quickly because errors did not reset the stall clock"
        );
    }

    // ── stall with errors and lag==0 recreates (group-gone scenario) ─────────

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stall_with_errors_and_zero_lag_recreates() {
        let recreate_counter = Arc::new(AtomicU64::new(0));
        let recreate_counter2 = Arc::clone(&recreate_counter);

        // lag == 0 (group gone) but we have errors
        let lag = Arc::new(AtomicU64::new(0));
        let lag2 = Arc::clone(&lag);

        let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
            recreate_counter2.fetch_add(1, Ordering::Relaxed);
            let l = Arc::clone(&lag2);
            Ok(Box::new(MockConsumer::<rb_schemas::AgentSessionCommand>::stalling(l))
                as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
        });

        // One immediate error, then stalls. lag == 0.
        let initial_consumer =
            MockConsumer::<rb_schemas::AgentSessionCommand>::erroring(Arc::clone(&lag), 1);

        let mut healthy = HealthyConsumer::with_factory(
            Box::new(initial_consumer),
            factory,
            WatchdogConfig {
                stall_timeout: Duration::from_millis(50),
                max_error_streak: 100, // disable streak path
            },
        );

        // Consume the error (sets consecutive_errors = 1).
        let r1 = healthy.next().await;
        assert!(matches!(r1, Some(Err(_))));

        // Consumer now stalls. Stall fires after 50ms. lag==0 but
        // consecutive_errors > 0 → must recreate (not loop silently).
        tokio::time::timeout(Duration::from_millis(300), healthy.next())
            .await
            .ok();

        assert!(
            recreate_counter.load(Ordering::Relaxed) >= 1,
            "should recreate when stall fires with lag==0 but consecutive_errors > 0"
        );
    }
}
