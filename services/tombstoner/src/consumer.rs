//! Kafka consumer loop for `rb.tombstones.v1`.
//!
//! Consumes [`Tombstone`] protobuf messages and removes all projections for the
//! indicated (tenant, repo) pair from `PostgreSQL`, `Neo4j`, and `Qdrant`.

use std::sync::Arc;

use anyhow::Result;
use metrics::counter;
use rb_kafka::{Consumer, ConsumerCfg, EventEnvelope};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::Tombstone;
use rb_storage_neo4j::TenantGraph;
use rb_storage_pg::TenantPool;
use tokio::task::JoinHandle;

use crate::delete;

const TOPIC_TOMBSTONES: &str = "rb.tombstones.v1";

/// Spawn the long-running consumer task.
///
/// Returns the [`JoinHandle`] so the caller can abort on shutdown.
#[allow(clippy::missing_errors_doc)]
pub fn spawn(
    pool: Arc<TenantPool>,
    graph: Arc<TenantGraph>,
    qdrant_url: Option<String>,
) -> Result<JoinHandle<()>> {
    let cfg = ConsumerCfg::new("tombstoner");
    let consumer = Consumer::<Tombstone>::new(&cfg)?;
    consumer.subscribe(&[TOPIC_TOMBSTONES])?;
    let mut consumer = KafkaHealthWatcher::wrap(
        consumer,
        &cfg,
        &[TOPIC_TOMBSTONES.to_owned()],
        WatchdogConfig::default(),
    );
    tracing::info!(
        kafka_health = "spawned",
        "tombstoner kafka health watchdog active"
    );

    let handle = tokio::spawn(async move {
        loop {
            match consumer.next().await {
                None => {
                    tracing::info!("tombstoner: stream ended");
                    break;
                }
                Some(Err(e)) => {
                    tracing::error!("tombstoner: kafka error: {e}");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Some(Ok(envelope)) => {
                    let commit = handle_envelope(
                        pool.as_ref(),
                        graph.as_ref(),
                        qdrant_url.as_deref(),
                        &envelope,
                    )
                    .await;
                    if commit {
                        if let Err(e) = consumer.commit(&envelope).await {
                            tracing::warn!("tombstoner: commit failed: {e}");
                        }
                    }
                }
            }
        }
    });

    Ok(handle)
}

/// Returns `true` if the caller should commit the offset.
async fn handle_envelope(
    pool: &TenantPool,
    graph: &TenantGraph,
    qdrant_url: Option<&str>,
    envelope: &EventEnvelope<Tombstone>,
) -> bool {
    let tenant_id = envelope.tenant_id;
    let ev = &envelope.payload;

    tracing::info!(
        tenant_id = %tenant_id,
        repo_id   = %ev.repo_id,
        requested_by = %ev.requested_by,
        "tombstoner: processing tombstone"
    );

    match delete::handle_tombstone(pool, graph, qdrant_url, &tenant_id, ev).await {
        Ok(()) => {
            counter!("rb_tombstoner_events_total", "outcome" => "ok").increment(1);
            tracing::info!(
                tenant_id = %tenant_id,
                repo_id   = %ev.repo_id,
                "tombstoner: projections deleted"
            );
            true
        }
        Err(e) => {
            tracing::error!(
                tenant_id = %tenant_id,
                repo_id   = %ev.repo_id,
                "tombstoner: deletion failed (transient): {e}"
            );
            counter!("rb_tombstoner_events_total", "outcome" => "err").increment(1);
            // Do not commit — Kafka will redeliver for retry.
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            false
        }
    }
}
