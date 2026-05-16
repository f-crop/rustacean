mod db;
mod sse;

#[cfg(test)]
use db::{TOTAL_PIPELINE_STAGES, stage_db_params};
#[cfg(test)]
use sse::{IngestStatusEventJson, stage_label, status_label};

use std::sync::{Arc, atomic::Ordering};

use anyhow::Result;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::IngestStatusEvent;
use rb_sse::EventBus;
use sqlx::PgPool;

use crate::state::KafkaConsistencyState;

/// Spawn the long-running Kafka consumer task that subscribes to
/// `rb.projector.events`, persists status transitions to Postgres, and fans
/// events out through the SSE bus.
///
/// Returns the [`tokio::task::JoinHandle`] so the caller can abort on shutdown.
///
/// # Errors
///
/// Returns an error if the Kafka consumer cannot be created or subscribed.
pub fn spawn(
    cfg: &ConsumerCfg,
    sse_bus: Arc<EventBus>,
    pool: Arc<PgPool>,
    consistency: Arc<KafkaConsistencyState>,
) -> Result<tokio::task::JoinHandle<()>> {
    let consumer = Consumer::<IngestStatusEvent>::new(cfg)?;
    consumer.subscribe(&["rb.projector.events"])?;
    let mut consumer = KafkaHealthWatcher::wrap(
        consumer,
        cfg,
        &["rb.projector.events".to_owned()],
        WatchdogConfig::default(),
    );

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("ingest_consumer: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("ingest_consumer: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    let now_ms = chrono::Utc::now().timestamp_millis();
                    consistency
                        .last_event_at_ms
                        .store(now_ms, Ordering::Relaxed);
                    consistency.lag_records.store(0, Ordering::Relaxed);

                    let ev = &envelope.payload;
                    let tenant_id = envelope.tenant_id;

                    // Reject malformed messages before hitting the DB. Legacy
                    // Kafka producers can emit an empty ingest_run_id; parsing
                    // that as a UUID would produce a misleading DB error and
                    // leave the offset uncommitted (causing replays on restart).
                    if ev.ingest_run_id.parse::<uuid::Uuid>().is_err() {
                        tracing::warn!(
                            raw_ingest_run_id = %ev.ingest_run_id,
                            tenant_id = %tenant_id,
                            "ingest_consumer: skipping malformed message (invalid ingest_run_id)"
                        );
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!("ingest_consumer: commit failed after skip: {e}");
                        }
                        continue;
                    }

                    if let Err(e) = db::handle_db_updates(&pool, ev).await {
                        tracing::error!(
                            ingest_run_id = %ev.ingest_run_id,
                            "ingest_consumer: DB update failed: {e}"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    let json_ev = sse::IngestStatusEventJson::from(ev);
                    match serde_json::to_string(&json_ev) {
                        Ok(data) => {
                            let span =
                                sse::sse_publish_span(&tenant_id, envelope.trace_context.as_ref());
                            span.in_scope(|| {
                                sse_bus.publish_raw(&tenant_id, "ingest.status", data);
                            });
                        }
                        Err(e) => {
                            tracing::error!("ingest_consumer: serialise error: {e}");
                        }
                    }

                    if let Err(e) = consumer.commit(&envelope).await {
                        tracing::warn!("ingest_consumer: commit failed: {e}");
                    }
                }
            }
        }
    });

    Ok(handle)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::{IngestStage, IngestStatus};
    use uuid::Uuid;

    // ── status_label ─────────────────────────────────────────────────────────

    #[test]
    fn status_label_unspecified() {
        assert_eq!(status_label(IngestStatus::Unspecified), "unspecified");
    }

    #[test]
    fn status_label_pending() {
        assert_eq!(status_label(IngestStatus::Pending), "pending");
    }

    #[test]
    fn status_label_processing() {
        assert_eq!(status_label(IngestStatus::Processing), "processing");
    }

    #[test]
    fn status_label_done() {
        assert_eq!(status_label(IngestStatus::Done), "done");
    }

    #[test]
    fn status_label_failed() {
        assert_eq!(status_label(IngestStatus::Failed), "failed");
    }

    // ── stage_label ──────────────────────────────────────────────────────────

    #[test]
    fn stage_label_unspecified_is_none() {
        assert!(stage_label(IngestStage::Unspecified).is_none());
    }

    #[test]
    fn stage_label_clone() {
        assert_eq!(stage_label(IngestStage::Clone), Some("clone"));
    }

    #[test]
    fn stage_label_expand() {
        assert_eq!(stage_label(IngestStage::Expand), Some("expand"));
    }

    #[test]
    fn stage_label_parse() {
        assert_eq!(stage_label(IngestStage::Parse), Some("parse"));
    }

    #[test]
    fn stage_label_typecheck() {
        assert_eq!(stage_label(IngestStage::Typecheck), Some("typecheck"));
    }

    #[test]
    fn stage_label_extract() {
        assert_eq!(stage_label(IngestStage::Extract), Some("extract"));
    }

    #[test]
    fn stage_label_embed() {
        assert_eq!(stage_label(IngestStage::Embed), Some("embed"));
    }

    #[test]
    fn stage_label_project_pg() {
        assert_eq!(stage_label(IngestStage::ProjectPg), Some("project_pg"));
    }

    #[test]
    fn stage_label_project_neo4j() {
        assert_eq!(
            stage_label(IngestStage::ProjectNeo4j),
            Some("project_neo4j")
        );
    }

    #[test]
    fn stage_label_project_qdrant() {
        assert_eq!(
            stage_label(IngestStage::ProjectQdrant),
            Some("project_qdrant")
        );
    }

    // ── stage_db_params ──────────────────────────────────────────────────────

    #[test]
    fn stage_db_params_processing() {
        let (status, err) = stage_db_params(IngestStatus::Processing, "").unwrap();
        assert_eq!(status, "running");
        assert!(err.is_none());
    }

    #[test]
    fn stage_db_params_done() {
        let (status, err) = stage_db_params(IngestStatus::Done, "").unwrap();
        assert_eq!(status, "succeeded");
        assert!(err.is_none());
    }

    #[test]
    fn stage_db_params_failed_with_message() {
        let (status, err) = stage_db_params(IngestStatus::Failed, "timeout").unwrap();
        assert_eq!(status, "failed");
        assert_eq!(err, Some("timeout".to_owned()));
    }

    #[test]
    fn stage_db_params_failed_empty_message() {
        let (status, err) = stage_db_params(IngestStatus::Failed, "").unwrap();
        assert_eq!(status, "failed");
        assert!(err.is_none());
    }

    #[test]
    fn stage_db_params_pending_is_none() {
        assert!(stage_db_params(IngestStatus::Pending, "").is_none());
    }

    #[test]
    fn stage_db_params_unspecified_is_none() {
        assert!(stage_db_params(IngestStatus::Unspecified, "").is_none());
    }

    // ── IngestStatusEventJson::from ───────────────────────────────────────────

    fn make_event(status: i32, stage: i32, error: &str) -> IngestStatusEvent {
        IngestStatusEvent {
            ingest_request_id: "req-1".to_owned(),
            tenant_id: "tenant-1".to_owned(),
            status,
            error_message: error.to_owned(),
            occurred_at_ms: 1_700_000_000_000,
            stage,
            stage_seq: 1,
            ingest_run_id: "run-1".to_owned(),
            attempt: 1,
        }
    }

    #[test]
    fn json_ev_processing_clone() {
        let ev = make_event(
            IngestStatus::Processing as i32,
            IngestStage::Clone as i32,
            "",
        );
        let j = IngestStatusEventJson::from(&ev);
        assert_eq!(j.status, "processing");
        assert_eq!(j.stage, Some("clone"));
        assert_eq!(j.ingest_run_id, "run-1");
        assert_eq!(j.stage_seq, 1);
    }

    #[test]
    fn json_ev_done_embed() {
        let ev = make_event(IngestStatus::Done as i32, IngestStage::Embed as i32, "");
        let j = IngestStatusEventJson::from(&ev);
        assert_eq!(j.status, "done");
        assert_eq!(j.stage, Some("embed"));
    }

    #[test]
    fn json_ev_failed_has_error() {
        let ev = make_event(
            IngestStatus::Failed as i32,
            IngestStage::Parse as i32,
            "oom",
        );
        let j = IngestStatusEventJson::from(&ev);
        assert_eq!(j.status, "failed");
        assert_eq!(j.error_message, "oom");
    }

    #[test]
    fn json_ev_unknown_status_label() {
        let ev = make_event(999, IngestStage::Clone as i32, "");
        let j = IngestStatusEventJson::from(&ev);
        assert_eq!(j.status, "unknown");
    }

    #[test]
    fn json_ev_unspecified_stage_is_none() {
        let ev = make_event(
            IngestStatus::Processing as i32,
            IngestStage::Unspecified as i32,
            "",
        );
        let j = IngestStatusEventJson::from(&ev);
        assert!(j.stage.is_none());
    }

    #[test]
    fn json_ev_serialises_to_valid_json() {
        let ev = make_event(IngestStatus::Done as i32, IngestStage::ProjectPg as i32, "");
        let j = IngestStatusEventJson::from(&ev);
        let s = serde_json::to_string(&j).expect("should serialise");
        let v: serde_json::Value = serde_json::from_str(&s).expect("should parse");
        assert_eq!(v["status"], "done");
        assert_eq!(v["stage"], "project_pg");
        assert_eq!(v["ingest_run_id"], "run-1");
    }

    // ── total stages constant ────────────────────────────────────────────────

    #[test]
    fn total_pipeline_stages_is_9() {
        assert_eq!(TOTAL_PIPELINE_STAGES, 9);
    }

    // ── UUID parsing edge cases ──────────────────────────────────────────────

    #[test]
    fn valid_uuid_parses_ok() {
        let id = Uuid::new_v4().to_string();
        assert!(id.parse::<Uuid>().is_ok());
    }

    #[test]
    fn invalid_uuid_errors() {
        assert!("not-a-uuid".parse::<Uuid>().is_err());
    }

    #[test]
    fn empty_ingest_run_id_fails_uuid_parse() {
        // The consumer guard relies on this to reject legacy malformed messages.
        assert!("".parse::<Uuid>().is_err());
    }

    // ── stage_db_params: all terminal states produce consistent output ────────

    #[test]
    fn all_non_terminal_statuses_produce_no_db_params() {
        for status in [IngestStatus::Pending, IngestStatus::Unspecified] {
            assert!(
                stage_db_params(status, "any error").is_none(),
                "{status:?} should not produce a DB update"
            );
        }
    }

    #[test]
    fn all_terminal_statuses_produce_db_params() {
        for status in [
            IngestStatus::Processing,
            IngestStatus::Done,
            IngestStatus::Failed,
        ] {
            assert!(
                stage_db_params(status, "").is_some(),
                "{status:?} should produce a DB update"
            );
        }
    }

    // ── all 9 stage labels are distinct and non-empty ────────────────────────

    #[test]
    fn all_named_stages_have_distinct_labels() {
        let stages = [
            IngestStage::Clone,
            IngestStage::Expand,
            IngestStage::Parse,
            IngestStage::Typecheck,
            IngestStage::Extract,
            IngestStage::Embed,
            IngestStage::ProjectPg,
            IngestStage::ProjectNeo4j,
            IngestStage::ProjectQdrant,
        ];

        let labels: Vec<&'static str> = stages.iter().filter_map(|s| stage_label(*s)).collect();
        assert_eq!(labels.len(), 9, "all 9 stages should have labels");

        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), 9, "stage labels must be unique");

        for label in &labels {
            assert!(!label.is_empty());
        }
    }
}
