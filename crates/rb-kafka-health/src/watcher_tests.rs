use super::*;
use rb_kafka::ConsumerCfg;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;

// ── Mock consumer ────────────────────────────────────────────────────────

type MsgQueue<E> = Mutex<Vec<Option<Result<EventEnvelope<E>, KafkaError>>>>;

struct MockConsumer<E: ProstMessage + Default + Send + Sync + 'static> {
    messages: MsgQueue<E>,
    lag: Arc<AtomicU64>,
    assignment_count: Arc<AtomicU64>,
}

impl<E: ProstMessage + Default + Send + Sync + 'static> MockConsumer<E> {
    /// Stalling consumer with 1 simulated assigned partition (normal state).
    fn stalling(lag: Arc<AtomicU64>) -> Self {
        Self {
            messages: Mutex::new(vec![]),
            lag,
            assignment_count: Arc::new(AtomicU64::new(1)),
        }
    }

    /// Stalling consumer with a configurable partition count (for group-loss tests).
    fn stalling_with_partitions(lag: Arc<AtomicU64>, partitions: Arc<AtomicU64>) -> Self {
        Self {
            messages: Mutex::new(vec![]),
            lag,
            assignment_count: partitions,
        }
    }

    /// Consumer that drains `messages` then stalls.
    fn with_messages(
        messages: Vec<Option<Result<EventEnvelope<E>, KafkaError>>>,
        lag: Arc<AtomicU64>,
    ) -> Self {
        Self {
            messages: Mutex::new(messages),
            lag,
            assignment_count: Arc::new(AtomicU64::new(1)),
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

    async fn nack_to_dlq(&self, _env: &EventEnvelope<E>, _reason: &str) -> Result<(), KafkaError> {
        Ok(())
    }

    async fn lag_estimate(&self) -> u64 {
        self.lag.load(Ordering::Relaxed)
    }

    async fn assigned_partition_count(&self) -> usize {
        #[allow(clippy::cast_possible_truncation)]
        {
            self.assignment_count.load(Ordering::Relaxed) as usize
        }
    }
}

#[allow(dead_code)]
fn dummy_cfg() -> ConsumerCfg {
    ConsumerCfg::new("test-group")
}

#[allow(clippy::unnecessary_wraps)]
fn make_stream_error<E: ProstMessage + Default + Send + Sync + 'static>()
-> Option<Result<EventEnvelope<E>, KafkaError>> {
    // Use a generic broker error to simulate any stream error (REQTMOUT cascade,
    // broker unavailable, etc.).  The watcher tracks ALL consecutive errors.
    Some(Err(KafkaError::Broker(
        "simulated: Local: Timed out".to_owned(),
    )))
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
            max_error_streak: 30,
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
            max_error_streak: 30,
        },
    );

    tokio::time::timeout(Duration::from_millis(200), healthy.next())
        .await
        .ok();

    assert_eq!(
        recreate_calls.load(Ordering::Relaxed),
        0,
        "factory must not be called when lag is zero and partitions are assigned"
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
            max_error_streak: 30,
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

// ── group membership loss: lag=0, no partitions, had_assignment=true ──────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn group_loss_triggers_recreate() {
    let recreate_counter = Arc::new(AtomicU64::new(0));
    let recreate_counter2 = Arc::clone(&recreate_counter);

    // lag=0 because there are no partitions to compute lag from
    let lag = Arc::new(AtomicU64::new(0));
    let lag2 = Arc::clone(&lag);
    // assignment_count=0: group evaporated from the broker
    let partitions = Arc::new(AtomicU64::new(0));
    let partitions2 = Arc::clone(&partitions);

    let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
        recreate_counter2.fetch_add(1, Ordering::Relaxed);
        let l = Arc::clone(&lag2);
        let p = Arc::clone(&partitions2);
        Ok(Box::new(MockConsumer::stalling_with_partitions(l, p))
            as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
    });

    let mut healthy = HealthyConsumer::with_factory(
        Box::new(MockConsumer::stalling_with_partitions(
            Arc::clone(&lag),
            Arc::clone(&partitions),
        )),
        factory,
        WatchdogConfig {
            stall_timeout: Duration::from_millis(50),
            max_error_streak: 30,
        },
    );

    // Simulate "we were receiving messages before the broker restart"
    healthy.had_assignment = true;

    tokio::time::timeout(Duration::from_millis(400), healthy.next())
        .await
        .ok();

    assert!(
        recreate_counter.load(Ordering::Relaxed) >= 1,
        "watchdog must recreate when group membership is lost (lag=0, partitions=0)"
    );
    assert!(
        healthy.recreate_count >= 1,
        "recreate_count should be >= 1 after group loss"
    );
}

// ── no spurious recreate on startup with no assignment ────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_recreate_on_startup_with_no_assignment() {
    let recreate_calls = Arc::new(AtomicU64::new(0));
    let recreate_calls2 = Arc::clone(&recreate_calls);

    let lag = Arc::new(AtomicU64::new(0));
    let partitions = Arc::new(AtomicU64::new(0)); // no partitions yet

    let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
        recreate_calls2.fetch_add(1, Ordering::Relaxed);
        Err(KafkaError::Broker("should not be called".into()))
    });

    // had_assignment stays false (default) — startup state
    let mut healthy = HealthyConsumer::with_factory(
        Box::new(MockConsumer::stalling_with_partitions(
            Arc::clone(&lag),
            Arc::clone(&partitions),
        )),
        factory,
        WatchdogConfig {
            stall_timeout: Duration::from_millis(30),
            max_error_streak: 30,
        },
    );

    tokio::time::timeout(Duration::from_millis(200), healthy.next())
        .await
        .ok();

    assert_eq!(
        recreate_calls.load(Ordering::Relaxed),
        0,
        "must not restart on startup when no assignment exists yet"
    );
}

// ── error cascade at threshold triggers recreate ──────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_cascade_at_threshold_triggers_recreate() {
    let recreate_counter = Arc::new(AtomicU64::new(0));
    let recreate_counter2 = Arc::clone(&recreate_counter);
    let lag = Arc::new(AtomicU64::new(0));
    let lag2 = Arc::clone(&lag);

    // threshold=1: first error immediately triggers restart
    let threshold: u32 = 1;
    let errors = vec![make_stream_error::<rb_schemas::AgentSessionCommand>()];

    let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
        recreate_counter2.fetch_add(1, Ordering::Relaxed);
        let l = Arc::clone(&lag2);
        Ok(Box::new(MockConsumer::stalling(l))
            as Box<dyn ConsumerOps<rb_schemas::AgentSessionCommand>>)
    });

    let mut healthy = HealthyConsumer::with_factory(
        Box::new(MockConsumer::with_messages(errors, Arc::clone(&lag))),
        factory,
        WatchdogConfig {
            stall_timeout: Duration::from_secs(60), // long — stall must not fire
            max_error_streak: threshold,
        },
    );

    tokio::time::timeout(Duration::from_millis(500), healthy.next())
        .await
        .ok();

    assert!(
        recreate_counter.load(Ordering::Relaxed) >= 1,
        "factory must be called after {threshold} consecutive error(s)"
    );
    assert!(
        healthy.recreate_count >= 1,
        "recreate_count should be >= 1 after error cascade"
    );
}

// ── errors below threshold: returned to caller, no recreate ──────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn errors_below_threshold_no_recreate() {
    let recreate_calls = Arc::new(AtomicU64::new(0));
    let recreate_calls2 = Arc::clone(&recreate_calls);
    let lag = Arc::new(AtomicU64::new(0));

    let threshold: u32 = 5;
    // Only 2 errors — below threshold
    let errors = vec![
        make_stream_error::<rb_schemas::AgentSessionCommand>(),
        make_stream_error::<rb_schemas::AgentSessionCommand>(),
    ];

    let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
        recreate_calls2.fetch_add(1, Ordering::Relaxed);
        Err(KafkaError::Broker("should not be called".into()))
    });

    let mut healthy = HealthyConsumer::with_factory(
        Box::new(MockConsumer::with_messages(errors, Arc::clone(&lag))),
        factory,
        WatchdogConfig {
            stall_timeout: Duration::from_secs(60),
            max_error_streak: threshold,
        },
    );

    // Consume both errors
    let r1 = tokio::time::timeout(Duration::from_millis(100), healthy.next()).await;
    assert!(
        matches!(r1, Ok(Some(Err(_)))),
        "first error should be returned to caller"
    );

    let r2 = tokio::time::timeout(Duration::from_millis(100), healthy.next()).await;
    assert!(
        matches!(r2, Ok(Some(Err(_)))),
        "second error should be returned to caller"
    );

    assert_eq!(
        recreate_calls.load(Ordering::Relaxed),
        0,
        "factory must not be called below threshold"
    );
    assert_eq!(healthy.recreate_count, 0);
    assert_eq!(
        healthy.consecutive_errors, 2,
        "consecutive_errors must track errors below threshold"
    );
}

// ── max_error_streak=0 disables error-cascade restart ────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn error_streak_zero_disables_cascade_restart() {
    let recreate_calls = Arc::new(AtomicU64::new(0));
    let recreate_calls2 = Arc::clone(&recreate_calls);
    let lag = Arc::new(AtomicU64::new(0));

    let errors = vec![
        make_stream_error::<rb_schemas::AgentSessionCommand>(),
        make_stream_error::<rb_schemas::AgentSessionCommand>(),
        make_stream_error::<rb_schemas::AgentSessionCommand>(),
    ];

    let factory: ConsumerFactory<rb_schemas::AgentSessionCommand> = Arc::new(move || {
        recreate_calls2.fetch_add(1, Ordering::Relaxed);
        Err(KafkaError::Broker("should not be called".into()))
    });

    let mut healthy = HealthyConsumer::with_factory(
        Box::new(MockConsumer::with_messages(errors, Arc::clone(&lag))),
        factory,
        WatchdogConfig {
            stall_timeout: Duration::from_secs(60),
            max_error_streak: 0, // disabled
        },
    );

    for _ in 0..3_u32 {
        tokio::time::timeout(Duration::from_millis(100), healthy.next())
            .await
            .ok();
    }

    assert_eq!(
        recreate_calls.load(Ordering::Relaxed),
        0,
        "max_error_streak=0 must disable error-cascade restart"
    );
}
