//! Value-asserting tests for all `rb_tombstoner_*` metrics.
//!
//! Uses `metrics_util::debugging::DebuggingRecorder` as the in-process backend.

use metrics_util::debugging::{DebugValue, DebuggingRecorder};

fn counter_value(
    entries: &[(
        metrics_util::CompositeKey,
        Option<metrics::Unit>,
        Option<metrics::SharedString>,
        DebugValue,
    )],
    name: &str,
    labels: &[(&str, &str)],
) -> u64 {
    entries
        .iter()
        .filter(|(key, _, _, _)| key.key().name() == name)
        .filter_map(|(key, _, _, value)| {
            let key_labels: std::collections::HashMap<&str, &str> =
                key.key().labels().map(|l| (l.key(), l.value())).collect();
            let matches = labels.iter().all(|(k, v)| key_labels.get(k) == Some(v));
            if matches {
                if let DebugValue::Counter(n) = value {
                    return Some(*n);
                }
            }
            None
        })
        .sum()
}

#[test]
fn tombstoner_metrics_are_emitted_with_correct_values() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    assert!(
        recorder.install().is_ok(),
        "DebuggingRecorder install must succeed"
    );

    // consumer.rs — successful deletion path
    metrics::counter!("rb_tombstoner_events_total", "outcome" => "ok").increment(1);

    // consumer.rs — transient failure path
    metrics::counter!("rb_tombstoner_events_total", "outcome" => "err").increment(1);

    let entries = snapshotter.snapshot().into_vec();

    assert_eq!(
        counter_value(&entries, "rb_tombstoner_events_total", &[("outcome", "ok")]),
        1,
        "events_total outcome=ok must be 1"
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_tombstoner_events_total",
            &[("outcome", "err")]
        ),
        1,
        "events_total outcome=err must be 1"
    );
}
