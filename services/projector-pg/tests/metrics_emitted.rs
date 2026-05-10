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
fn rb_projector_pg_events_total_metric_is_emitted() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    assert!(
        recorder.install().is_ok(),
        "DebuggingRecorder install must succeed"
    );

    metrics::counter!("rb_projector_pg_events_total", "event_type" => "source_file", "outcome" => "ok").increment(1);
    metrics::counter!("rb_projector_pg_events_total", "event_type" => "parsed_item", "outcome" => "ok").increment(1);
    metrics::counter!("rb_projector_pg_events_total", "event_type" => "relation", "outcome" => "ok").increment(1);
    metrics::counter!("rb_projector_pg_events_total", "event_type" => "source_file", "outcome" => "err").increment(1);
    metrics::counter!("rb_projector_pg_events_total", "event_type" => "parsed_item", "outcome" => "err").increment(1);
    metrics::counter!("rb_projector_pg_events_total", "event_type" => "relation", "outcome" => "err").increment(1);

    let entries = snapshotter.snapshot().into_vec();

    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "source_file"), ("outcome", "ok")]
        ),
        1,
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "parsed_item"), ("outcome", "ok")]
        ),
        1,
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "relation"), ("outcome", "ok")]
        ),
        1,
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "source_file"), ("outcome", "err")]
        ),
        1,
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "parsed_item"), ("outcome", "err")]
        ),
        1,
    );
    assert_eq!(
        counter_value(
            &entries,
            "rb_projector_pg_events_total",
            &[("event_type", "relation"), ("outcome", "err")]
        ),
        1,
    );
}
