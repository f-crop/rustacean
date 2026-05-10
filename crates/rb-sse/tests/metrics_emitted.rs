//! K-HIGH-1 (rb-sse): Assert all required `rb_sse_*` metric names are emitted
//! with correct semantics.
//!
//! Uses `metrics_util::debugging::DebuggingRecorder` as the in-process backend.
//! All three metrics are exercised in a single test to avoid global-recorder
//! conflicts between concurrent tokio test runners.

use futures::StreamExt as _;
use metrics_util::debugging::DebuggingRecorder;
use rb_sse::{EventBus, SseConfig, TenantId};

#[tokio::test]
async fn all_required_metric_names_are_emitted() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    let _ = recorder.install();

    // ── rb_sse_clients + rb_sse_events_dispatched_total ───────────────────────
    let bus = EventBus::new(SseConfig::default());
    let tenant = TenantId::new();

    // subscribe_with_cfg → subscribe_raw → increments rb_sse_clients
    let mut stream = bus.subscribe_with_cfg(&tenant, None, &SseConfig::default());

    // publish_raw → increments rb_sse_events_dispatched_total
    bus.publish_raw(&tenant, "test.event", r#"{"n":1}"#.to_owned());

    // Drive the stream once so the event is processed.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), stream.next()).await;

    // ── rb_sse_dropped_total ──────────────────────────────────────────────────
    // Small channel capacity forces the subscriber to fall behind (Lagged).
    let lag_cfg = SseConfig {
        channel_capacity: 2,
        ..Default::default()
    };
    let lag_bus = EventBus::new(lag_cfg.clone());
    let lag_tenant = TenantId::new();

    // Subscribe but do not drain; holds position 0 in the channel.
    let mut lag_stream = lag_bus.subscribe_with_cfg(&lag_tenant, None, &lag_cfg);

    // Publish more events than the channel can hold — subscriber lags.
    for i in 0..5u32 {
        lag_bus.publish_raw(&lag_tenant, "ev", i.to_string());
    }

    // Polling the stream triggers RecvError::Lagged(n) → rb_sse_dropped_total.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), lag_stream.next()).await;

    // ── Assertions ────────────────────────────────────────────────────────────
    let snapshot = snapshotter.snapshot();
    let metric_names: Vec<String> = snapshot
        .into_vec()
        .into_iter()
        .map(|(composite_key, _, _, _)| composite_key.key().name().to_owned())
        .collect();

    let required = [
        "rb_sse_clients",
        "rb_sse_events_dispatched_total",
        "rb_sse_dropped_total",
    ];
    for name in &required {
        assert!(
            metric_names.iter().any(|n| n == *name),
            "metric '{name}' was not emitted; found: {metric_names:?}",
        );
    }
}
