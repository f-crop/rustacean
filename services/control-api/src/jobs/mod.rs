//! Background and startup jobs for control-api.

use std::time::Duration;

use rb_kafka::EventEnvelope;
use rb_schemas::{IngestRequest, TenantId};
use uuid::Uuid;

use crate::state::AppState;

const CLONE_COMMANDS_TOPIC: &str = "rb.ingest.clone.commands";

/// Interval between reconciler passes.
const RECONCILER_INTERVAL: Duration = Duration::from_secs(120);

struct OrphanedRun {
    id: Uuid,
    tenant_id: Uuid,
    repo_id: Uuid,
    commit_sha: Option<String>,
}

/// Spawns a background tokio task that calls [`reconcile_orphaned_ingest_runs`]
/// every 2 minutes.
///
/// The first tick fires immediately so recently-stuck runs are healed without
/// waiting a full interval.  Missed ticks are skipped (no burst catch-up) to
/// prevent thundering-herd on a recovering Kafka broker.
///
/// The returned [`tokio::task::JoinHandle`] can be kept for graceful shutdown
/// or dropped — dropping does not cancel the task.
pub fn spawn_reconciler_loop(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(RECONCILER_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            reconcile_orphaned_ingest_runs(&state).await;
        }
    })
}

/// Re-dispatch any ingestion runs that are stuck in `queued` for more than
/// 2 minutes without a Kafka message being produced.
///
/// Called periodically by [`spawn_reconciler_loop`].  Recovers from the
/// failure mode where control-api crashed or was restarted after the DB row
/// was committed but before (or during) the Kafka publish.  The trigger
/// handler normally rolls back the transaction on Kafka failure, but a
/// mid-flight process death can leave an orphan.
///
/// Recovery strategy:
/// - Kafka producer available → re-publish `IngestRequest` with a new
///   `event_id`; the clone worker processes it normally.
/// - Kafka producer unavailable → mark the run `failed` with a clear
///   error so the user can re-trigger without manual DB surgery.
///
/// Bounded: processes at most 100 runs per pass (oldest first) to cap
/// work per interval after a prolonged outage.
pub async fn reconcile_orphaned_ingest_runs(state: &AppState) {
    let runs = match fetch_orphaned_runs(&state.pool).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "reconciler: failed to query orphaned runs");
            return;
        }
    };

    if runs.is_empty() {
        tracing::debug!("reconciler: no orphaned queued ingestion runs");
        return;
    }

    tracing::warn!(
        count = runs.len(),
        "reconciler: found orphaned queued ingestion runs — recovering"
    );

    for run in runs {
        handle_orphaned_run(state, run).await;
    }
}

async fn fetch_orphaned_runs(pool: &sqlx::PgPool) -> Result<Vec<OrphanedRun>, sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, tenant_id, repo_id, commit_sha \
         FROM control.ingestion_runs \
         WHERE status = 'queued' \
           AND started_at IS NULL \
           AND created_at < now() - interval '2 minutes' \
         ORDER BY created_at ASC \
         LIMIT 100",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(id, tenant_id, repo_id, commit_sha)| OrphanedRun {
            id,
            tenant_id,
            repo_id,
            commit_sha,
        })
        .collect())
}

/// Attempt to claim an orphaned run by stamping `started_at = now()`.
///
/// Returns `true` if this process won the race (the row had `started_at IS NULL`
/// at the time of the UPDATE), `false` if another instance already claimed it.
/// This gives mutual exclusion when multiple control-api pods restart
/// simultaneously and all try to re-dispatch the same orphan.
///
/// The clone worker uses `COALESCE(started_at, now())` in `maybe_start_run`,
/// so a pre-set `started_at` is preserved without a separate migration.
async fn try_claim_run(pool: &sqlx::PgPool, run_id: Uuid) -> Result<bool, sqlx::Error> {
    let rows_affected = sqlx::query(
        "UPDATE control.ingestion_runs \
         SET started_at = now() \
         WHERE id = $1 AND status = 'queued' AND started_at IS NULL",
    )
    .bind(run_id)
    .execute(pool)
    .await?
    .rows_affected();

    Ok(rows_affected > 0)
}

async fn handle_orphaned_run(state: &AppState, run: OrphanedRun) {
    // Claim before dispatching — prevents double-publish when multiple
    // control-api instances restart simultaneously.
    match try_claim_run(&state.pool, run.id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::debug!(
                run_id = %run.id,
                "reconciler: run already claimed by another instance; skipping"
            );
            return;
        }
        Err(e) => {
            tracing::error!(
                run_id = %run.id,
                error = %e,
                "reconciler: failed to claim run; skipping"
            );
            return;
        }
    }

    if let Some(producer) = &state.ingest_producer {
        // Probe broker before building the envelope so we fall through
        // to the fail path quickly instead of waiting the full delivery
        // timeout (120 s) on a dead broker.
        if !producer.check_ready(Duration::from_millis(500)).await {
            tracing::warn!(
                run_id = %run.id,
                tenant_id = %run.tenant_id,
                "reconciler: Kafka broker unreachable; marking run failed"
            );
            mark_failed(
                &state.pool,
                run.id,
                "reconciler: Kafka broker unreachable on recovery; re-trigger to start ingestion",
            )
            .await;
            return;
        }

        let event_id = Uuid::new_v4();
        let ingest_req = IngestRequest {
            tenant_id: run.tenant_id.to_string(),
            event_id: event_id.to_string(),
            source: "reconciler".to_string(),
            payload: vec![],
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            repo_id: run.repo_id.to_string(),
            ingest_run_id: run.id.to_string(),
            // Preserve the commit SHA from the original request.  Empty
            // branch tells the clone stage to use the repo's default branch.
            commit_sha: run.commit_sha.unwrap_or_default(),
            branch: String::new(),
        };
        let envelope =
            EventEnvelope::new(TenantId::from(run.tenant_id), ingest_req).with_event_id(event_id);
        let partition_key = format!("{}.{}", run.tenant_id, run.repo_id);

        match producer
            .publish(CLONE_COMMANDS_TOPIC, partition_key.as_bytes(), envelope)
            .await
        {
            Ok(_) => {
                tracing::info!(
                    run_id = %run.id,
                    tenant_id = %run.tenant_id,
                    repo_id = %run.repo_id,
                    "reconciler: re-published orphaned ingestion run"
                );
            }
            Err(e) => {
                tracing::warn!(
                    run_id = %run.id,
                    error = %e,
                    "reconciler: re-publish failed; marking run failed"
                );
                mark_failed(
                    &state.pool,
                    run.id,
                    "reconciler: Kafka publish failed during recovery; re-trigger to start ingestion",
                )
                .await;
            }
        }
    } else {
        tracing::warn!(
            run_id = %run.id,
            tenant_id = %run.tenant_id,
            repo_id = %run.repo_id,
            "reconciler: no Kafka producer configured; marking orphaned run failed"
        );
        mark_failed(
            &state.pool,
            run.id,
            "reconciler: control-api restarted with no Kafka producer before message was published; re-trigger to start ingestion",
        )
        .await;
    }
}

async fn mark_failed(pool: &sqlx::PgPool, run_id: Uuid, error: &str) {
    if let Err(e) = sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'failed', finished_at = now(), error = $2 \
         WHERE id = $1 AND status = 'queued'",
    )
    .bind(run_id)
    .bind(error)
    .execute(pool)
    .await
    {
        tracing::error!(
            run_id = %run_id,
            error = %e,
            "reconciler: failed to mark run as failed"
        );
    }
}

#[cfg(test)]
mod tests;
