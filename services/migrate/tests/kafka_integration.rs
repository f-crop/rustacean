/// Integration tests for the Kafka migration runner.
///
/// Requires a running Kafka broker. Set `TEST_KAFKA_BROKERS` to run them.
/// The compose/test.yml stack (port 9093) provides the test broker:
///   docker compose -f compose/test.yml up -d kafka
///   `TEST_KAFKA_BROKERS=localhost:9093` cargo test -p migrate kafka
///
/// Note: apache/kafka:3.9 in test.yml advertises `PLAINTEXT://kafka:9092` internally.
/// For host-based testing, add `127.0.0.1 kafka` to /etc/hosts or run from inside
/// the Docker network where `kafka` resolves correctly.
use std::fmt::Write as _;
use std::io::Write;

use migrate::{KafkaAdmin, apply_topics, load_topics_file, print_status};
use tempfile::NamedTempFile;

fn test_brokers() -> Option<String> {
    std::env::var("TEST_KAFKA_BROKERS").ok()
}

fn unique_topic(base: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!("{base}.test.{ns}")
}

fn make_topics_yaml(topics: &[(&str, i32, &str)]) -> NamedTempFile {
    let mut content = "topics:\n".to_owned();
    for (name, partitions, retention) in topics {
        write!(content, "  - name: {name}\n    partitions: {partitions}\n    replication_factor: 1\n    config:\n      retention.ms: \"{retention}\"\n").expect("infallible write to String");
    }
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f
}

// ── unit tests (no broker required) ─────────────────────────────────────────

#[test]
fn test_load_all_27_topics_from_infra_yaml() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("infra")
        .join("kafka")
        .join("topics.yaml");

    let tf = load_topics_file(&path).expect("infra/kafka/topics.yaml must be loadable");
    assert_eq!(
        tf.topics.len(),
        27,
        "expected 27 topic definitions (8 base + 2 payload + 6 retry + 7 DLQ + 4 agent)"
    );

    let names: Vec<&str> = tf.topics.iter().map(|t| t.name.as_str()).collect();

    // Base topics
    assert!(names.contains(&"rb.ingest.clone.commands"));
    assert!(names.contains(&"rb.ingest.expand.commands"));
    assert!(names.contains(&"rb.ingest.parse.commands"));
    assert!(names.contains(&"rb.ingest.typecheck.commands"));
    assert!(names.contains(&"rb.ingest.graph.commands"));
    assert!(names.contains(&"rb.ingest.embed.commands"));
    assert!(names.contains(&"rb.projector.events"));
    assert!(names.contains(&"rb.audit.events"));
    // Payload topics
    assert!(names.contains(&"rb.parsed-items.v1"));
    assert!(names.contains(&"rb.typechecked-items.v1"));

    // Retry topics (one per pipeline stage)
    assert!(names.contains(&"rb.ingest.clone.commands.retry"));
    assert!(names.contains(&"rb.ingest.parse.commands.retry"));
    assert!(names.contains(&"rb.ingest.embed.commands.retry"));

    // DLQ topics (one per pipeline stage + projector)
    assert!(names.contains(&"rb.ingest.clone.commands.dlq"));
    assert!(names.contains(&"rb.ingest.parse.commands.dlq"));
    assert!(names.contains(&"rb.projector.events.dlq"));

    // Agent topics (ADR-009 process-spawning)
    assert!(names.contains(&"rb.agent.commands"));
    assert!(names.contains(&"rb.agent.commands.retry"));
    assert!(names.contains(&"rb.agent.commands.dlq"));
    assert!(names.contains(&"rb.agent.events"));
}

#[test]
fn test_projector_has_12_partitions() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("infra")
        .join("kafka")
        .join("topics.yaml");

    let tf = load_topics_file(&path).unwrap();
    let proj = tf
        .topics
        .iter()
        .find(|t| t.name == "rb.projector.events")
        .unwrap();
    assert_eq!(proj.partitions, 12);
}

#[test]
fn test_audit_has_90_day_retention() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("infra")
        .join("kafka")
        .join("topics.yaml");

    let tf = load_topics_file(&path).unwrap();
    let audit = tf
        .topics
        .iter()
        .find(|t| t.name == "rb.audit.events")
        .unwrap();
    assert_eq!(
        audit.config.get("retention.ms").map(String::as_str),
        Some("7776000000")
    );
}

// ── integration tests (require TEST_KAFKA_BROKERS) ───────────────────────────

#[tokio::test]
async fn test_create_topics_and_idempotent_rerun() {
    let Some(brokers) = test_brokers() else {
        eprintln!("SKIP test_create_topics_and_idempotent_rerun: TEST_KAFKA_BROKERS not set");
        return;
    };

    let t1 = unique_topic("rb.test.create");
    let t2 = unique_topic("rb.test.create2");
    let yaml = make_topics_yaml(&[(&t1, 3, "86400000"), (&t2, 6, "172800000")]);

    // First run: should create both topics
    let r1 = apply_topics(&brokers, yaml.path())
        .await
        .expect("first apply failed");
    assert_eq!(r1.created, 2, "expected 2 topics created on first run");

    // Second run: idempotent — no new topics
    let r2 = apply_topics(&brokers, yaml.path())
        .await
        .expect("second apply failed");
    assert_eq!(
        r2.created, 0,
        "expected 0 topics created on second run (idempotent)"
    );
    assert_eq!(
        r2.configs_applied, 2,
        "expected configs re-applied on second run"
    );
}

#[tokio::test]
async fn test_status_shows_missing_topic() {
    let Some(brokers) = test_brokers() else {
        eprintln!("SKIP test_status_shows_missing_topic: TEST_KAFKA_BROKERS not set");
        return;
    };

    let name = unique_topic("rb.test.nonexistent");
    let yaml = make_topics_yaml(&[(&name, 1, "3600000")]);

    let admin = KafkaAdmin::new(&brokers).expect("admin client creation failed");
    let tf = load_topics_file(yaml.path()).unwrap();
    let statuses = admin.status(&tf).expect("status call failed");

    assert_eq!(statuses.len(), 1);
    assert!(!statuses[0].exists, "topic should not exist yet");
    assert!(statuses[0].live_partitions.is_none());
}

#[tokio::test]
async fn test_status_shows_existing_topic() {
    let Some(brokers) = test_brokers() else {
        eprintln!("SKIP test_status_shows_existing_topic: TEST_KAFKA_BROKERS not set");
        return;
    };

    let name = unique_topic("rb.test.status");
    let yaml = make_topics_yaml(&[(&name, 4, "3600000")]);

    apply_topics(&brokers, yaml.path())
        .await
        .expect("apply failed");

    let admin = KafkaAdmin::new(&brokers).expect("admin client creation failed");
    let tf = load_topics_file(yaml.path()).unwrap();
    let statuses = admin.status(&tf).expect("status call failed");

    assert!(statuses[0].exists, "topic should exist after apply");
    assert_eq!(statuses[0].live_partitions, Some(4));
}

#[tokio::test]
async fn test_print_status_does_not_panic() {
    let Some(brokers) = test_brokers() else {
        eprintln!("SKIP test_print_status_does_not_panic: TEST_KAFKA_BROKERS not set");
        return;
    };

    let name = unique_topic("rb.test.print");
    let yaml = make_topics_yaml(&[(&name, 2, "3600000")]);

    print_status(&brokers, yaml.path()).expect("print_status must not fail");
}
