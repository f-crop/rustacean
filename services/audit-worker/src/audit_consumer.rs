//! Consumes `rb.audit.events` and writes each event to `audit.audit_events`.

use std::sync::Arc;

use anyhow::Result;
use chrono::DateTime;
use rb_kafka::{Consumer, ConsumerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::AuditEvent;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub fn spawn(pool: &Arc<PgPool>) -> Result<JoinHandle<()>> {
    let cfg = ConsumerCfg::new("audit-worker-audit-events");
    let consumer = Consumer::<AuditEvent>::new(&cfg)?;
    consumer.subscribe(&["rb.audit.events"])?;
    let mut consumer = KafkaHealthWatcher::wrap(
        consumer,
        &cfg,
        &["rb.audit.events".to_owned()],
        WatchdogConfig::default(),
    );
    tracing::info!(
        kafka_health = "spawned",
        "audit_consumer kafka health watchdog active"
    );
    let pool = Arc::clone(pool);

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("audit_consumer: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("audit_consumer: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    let ev = &envelope.payload;
                    if let Err(e) = insert_audit_event(&pool, ev).await {
                        tracing::error!(
                            event_id = %ev.event_id,
                            tenant_id = %ev.tenant_id,
                            action = %ev.action,
                            "audit_consumer: db insert failed: {e}"
                        );
                        // Don't commit on db error — retry on restart.
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        continue;
                    }
                    if let Err(e) = consumer.commit(&envelope).await {
                        tracing::warn!("audit_consumer: commit failed: {e}");
                    }
                }
            }
        }
    });

    Ok(handle)
}

async fn insert_audit_event(pool: &PgPool, ev: &AuditEvent) -> Result<()> {
    let event_id = ev
        .event_id
        .parse::<Uuid>()
        .unwrap_or_else(|_| Uuid::new_v4());
    let tenant_id = ev.tenant_id.parse::<Uuid>().unwrap_or_else(|_| Uuid::nil());
    let ingestion_run_id: Option<Uuid> = ev.ingestion_run_id.parse().ok();
    let repo_id: Option<Uuid> = ev.repo_id.parse().ok();
    let actor_user_id: Option<Uuid> = ev.actor_user_id.parse().ok();
    let stage: Option<&str> = if ev.stage.is_empty() {
        None
    } else {
        Some(&ev.stage)
    };
    let stage_seq: Option<i32> = if ev.stage_seq == 0 {
        None
    } else {
        Some(ev.stage_seq)
    };
    let occurred_at =
        DateTime::from_timestamp_millis(ev.occurred_at_ms).unwrap_or_else(chrono::Utc::now);
    let payload: serde_json::Value = serde_json::from_slice(&ev.payload)
        .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));

    // ON CONFLICT DO NOTHING prevents double-write on redelivery (idempotent via unique index).
    sqlx::query(
        "INSERT INTO audit.audit_events \
         (event_id, tenant_id, ingestion_run_id, repo_id, stage, stage_seq, \
          actor_kind, actor_user_id, action, outcome, occurred_at, payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (tenant_id, event_id) DO NOTHING",
    )
    .bind(event_id)
    .bind(tenant_id)
    .bind(ingestion_run_id)
    .bind(repo_id)
    .bind(stage)
    .bind(stage_seq)
    .bind(&ev.actor_kind)
    .bind(actor_user_id)
    .bind(&ev.action)
    .bind(&ev.outcome)
    .bind(occurred_at)
    .bind(payload)
    .execute(pool)
    .await?;

    tracing::debug!(
        %event_id,
        %tenant_id,
        action = %ev.action,
        outcome = %ev.outcome,
        "audit event recorded"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_uuid_fallback_on_invalid() {
        let id = "not-a-uuid".parse::<Uuid>();
        assert!(id.is_err());
        let fallback = id.unwrap_or_else(|_| Uuid::new_v4());
        assert!(!fallback.is_nil());
    }

    #[test]
    fn optional_uuid_none_on_empty() {
        let id: Option<Uuid> = "".parse().ok();
        assert!(id.is_none());
    }

    #[test]
    fn optional_uuid_some_on_valid() {
        let valid = Uuid::new_v4().to_string();
        let id: Option<Uuid> = valid.parse().ok();
        assert!(id.is_some());
    }

    #[test]
    fn stage_empty_becomes_none() {
        let stage = "";
        let opt: Option<&str> = if stage.is_empty() { None } else { Some(stage) };
        assert!(opt.is_none());
    }

    #[test]
    fn stage_seq_zero_becomes_none() {
        let seq: i32 = 0;
        let opt: Option<i32> = if seq == 0 { None } else { Some(seq) };
        assert!(opt.is_none());
    }

    #[test]
    fn payload_invalid_json_falls_back_to_empty_object() {
        let bad: &[u8] = b"not json";
        let val: serde_json::Value = serde_json::from_slice(bad)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::default()));
        assert!(val.as_object().is_some_and(serde_json::Map::is_empty));
    }

    #[test]
    fn occurred_at_from_millis_fallback() {
        // Overflow value — should fall back to now().
        let ts = DateTime::from_timestamp_millis(i64::MAX);
        assert!(ts.is_none());
    }
}
