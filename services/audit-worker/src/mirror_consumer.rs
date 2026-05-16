//! Mirrors `IngestStatusEvent` from `rb.projector.events` into `audit.audit_events`.
//!
//! This creates an immutable audit trail of ingest lifecycle transitions even
//! before stage workers start emitting to `rb.audit.events` directly.

use std::sync::Arc;

use anyhow::Result;
use chrono::DateTime;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{IngestStatus, IngestStatusEvent};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub fn spawn(pool: &Arc<PgPool>) -> Result<JoinHandle<()>> {
    let cfg = ConsumerCfg::new("audit-worker-projector-mirror");
    let consumer = Consumer::<IngestStatusEvent>::new(&cfg)?;
    consumer.subscribe(&["rb.projector.events"])?;
    let mut consumer = KafkaHealthWatcher::wrap(
        consumer,
        &cfg,
        &["rb.projector.events".to_owned()],
        WatchdogConfig::default(),
    );
    tracing::info!(
        kafka_health = "spawned",
        "mirror_consumer kafka health watchdog active"
    );
    let pool = Arc::clone(pool);

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("mirror_consumer: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("mirror_consumer: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    let ev = &envelope.payload;
                    let action = status_to_action(ev.status);
                    let outcome = if ev.status == IngestStatus::Failed as i32 {
                        "failure"
                    } else {
                        "success"
                    };

                    if let Err(e) = insert_status_mirror(&pool, ev, action, outcome).await {
                        tracing::error!(
                            ingest_request_id = %ev.ingest_request_id,
                            tenant_id = %ev.tenant_id,
                            action = %action,
                            "mirror_consumer: db insert failed: {e}"
                        );
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }

                    if let Err(e) = consumer.commit(&envelope).await {
                        tracing::warn!("mirror_consumer: commit failed: {e}");
                    }
                }
            }
        }
    });

    Ok(handle)
}

/// Map an `IngestStatus` int to the canonical action string used in audit events.
fn status_to_action(status: i32) -> &'static str {
    match IngestStatus::try_from(status).unwrap_or(IngestStatus::Unspecified) {
        IngestStatus::Unspecified => "ingest.status.unspecified",
        IngestStatus::Pending => "ingest.status.pending",
        IngestStatus::Processing => "ingest.stage.started",
        IngestStatus::Done => "ingest.stage.completed",
        IngestStatus::Failed => "ingest.stage.failed",
    }
}

async fn insert_status_mirror(
    pool: &PgPool,
    ev: &IngestStatusEvent,
    action: &str,
    outcome: &str,
) -> Result<()> {
    let tenant_id: Uuid = ev.tenant_id.parse().unwrap_or_else(|_| Uuid::nil());
    let ingest_run_id: Option<Uuid> = ev.ingest_request_id.parse().ok();
    let occurred_at =
        DateTime::from_timestamp_millis(ev.occurred_at_ms).unwrap_or_else(chrono::Utc::now);

    let payload = serde_json::json!({
        "ingest_request_id": ev.ingest_request_id,
        "status": ev.status,
        "error_message": ev.error_message,
    });

    // Use (tenant_id + ingest_request_id + action) as a deterministic idempotency
    // key by deriving a v5 UUID. This avoids duplicate rows on consumer restart.
    let idempotency_ns = uuid::Uuid::NAMESPACE_OID;
    let key_str = format!("{tenant_id}:{action}:{}", ev.ingest_request_id);
    let event_id = Uuid::new_v5(&idempotency_ns, key_str.as_bytes());

    sqlx::query(
        "INSERT INTO audit.audit_events \
         (event_id, tenant_id, ingestion_run_id, actor_kind, action, \
          outcome, occurred_at, payload) \
         VALUES ($1, $2, $3, 'system', $4, $5, $6, $7) \
         ON CONFLICT (tenant_id, event_id) DO NOTHING",
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(ingest_run_id)
    .bind(action)
    .bind(outcome)
    .bind(occurred_at)
    .bind(payload)
    .execute(pool)
    .await?;

    tracing::debug!(
        %event_id,
        %tenant_id,
        %action,
        "mirror audit event recorded"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rb_schemas::IngestStatus;

    #[test]
    fn processing_maps_to_started() {
        assert_eq!(
            status_to_action(IngestStatus::Processing as i32),
            "ingest.stage.started"
        );
    }

    #[test]
    fn done_maps_to_completed() {
        assert_eq!(
            status_to_action(IngestStatus::Done as i32),
            "ingest.stage.completed"
        );
    }

    #[test]
    fn failed_maps_to_failed() {
        assert_eq!(
            status_to_action(IngestStatus::Failed as i32),
            "ingest.stage.failed"
        );
    }

    #[test]
    fn failed_outcome_is_failure() {
        let status = IngestStatus::Failed as i32;
        let outcome = if status == IngestStatus::Failed as i32 {
            "failure"
        } else {
            "success"
        };
        assert_eq!(outcome, "failure");
    }

    #[test]
    fn done_outcome_is_success() {
        let status = IngestStatus::Done as i32;
        let outcome = if status == IngestStatus::Failed as i32 {
            "failure"
        } else {
            "success"
        };
        assert_eq!(outcome, "success");
    }

    #[test]
    fn idempotency_key_is_deterministic() {
        let tenant_id = Uuid::new_v4();
        let action = "ingest.stage.completed";
        let request_id = "req-abc";
        let ns = uuid::Uuid::NAMESPACE_OID;
        let key1 = format!("{tenant_id}:{action}:{request_id}");
        let key2 = format!("{tenant_id}:{action}:{request_id}");
        let id1 = Uuid::new_v5(&ns, key1.as_bytes());
        let id2 = Uuid::new_v5(&ns, key2.as_bytes());
        assert_eq!(id1, id2);
    }
}
