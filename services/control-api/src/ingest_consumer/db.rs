//! Database update functions for ingest status fan-out.

use anyhow::{Context as _, Result};
use rb_schemas::{IngestStage, IngestStatus, IngestStatusEvent};
use sqlx::PgPool;
use uuid::Uuid;

use super::sse::stage_label;

pub(crate) const TOTAL_PIPELINE_STAGES: i64 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processing_maps_to_running() {
        let result = stage_db_params(IngestStatus::Processing, "");
        assert_eq!(result, Some(("running", None)));
    }

    #[test]
    fn done_maps_to_succeeded() {
        assert_eq!(
            stage_db_params(IngestStatus::Done, ""),
            Some(("succeeded", None))
        );
    }

    #[test]
    fn pending_and_unspecified_produce_no_db_update() {
        assert!(stage_db_params(IngestStatus::Pending, "").is_none());
        assert!(stage_db_params(IngestStatus::Unspecified, "").is_none());
    }
}

/// Returns the `pipeline_stage_runs.status` string and optional error
/// for a given [`IngestStatus`], or `None` if no DB update is warranted.
pub(crate) fn stage_db_params(
    status: IngestStatus,
    error_message: &str,
) -> Option<(&'static str, Option<String>)> {
    match status {
        IngestStatus::Processing => Some(("running", None)),
        IngestStatus::Done => Some(("succeeded", None)),
        IngestStatus::Failed => {
            let err = if error_message.is_empty() {
                None
            } else {
                Some(error_message.to_owned())
            };
            Some(("failed", err))
        }
        IngestStatus::Pending | IngestStatus::Unspecified => None,
    }
}

/// Update `pipeline_stage_runs` for the given run + stage transition.
pub(crate) async fn update_stage_run(
    pool: &PgPool,
    ingest_run_id: &str,
    stage: &str,
    db_status: &str,
    error: Option<String>,
) -> Result<()> {
    let run_id: Uuid = ingest_run_id
        .parse()
        .context("invalid ingest_run_id UUID")?;

    match db_status {
        "running" => {
            sqlx::query(
                "UPDATE control.pipeline_stage_runs \
                 SET status = 'running', started_at = COALESCE(started_at, now()) \
                 WHERE ingestion_run_id = $1 AND stage = $2",
            )
            .bind(run_id)
            .bind(stage)
            .execute(pool)
            .await
            .context("failed to update pipeline_stage_runs to running")?;
        }
        "succeeded" => {
            sqlx::query(
                "UPDATE control.pipeline_stage_runs \
                 SET status = 'succeeded', finished_at = now() \
                 WHERE ingestion_run_id = $1 AND stage = $2",
            )
            .bind(run_id)
            .bind(stage)
            .execute(pool)
            .await
            .context("failed to update pipeline_stage_runs to succeeded")?;
        }
        "failed" => {
            sqlx::query(
                "UPDATE control.pipeline_stage_runs \
                 SET status = 'failed', finished_at = now(), error = $3 \
                 WHERE ingestion_run_id = $1 AND stage = $2",
            )
            .bind(run_id)
            .bind(stage)
            .bind(error.as_deref())
            .execute(pool)
            .await
            .context("failed to update pipeline_stage_runs to failed")?;
        }
        other => {
            tracing::warn!(db_status = other, "unknown stage db_status — skipping");
        }
    }

    Ok(())
}

/// Transition `ingestion_runs` when a stage reports `Processing` (first signal
/// that work has started: move from `queued` → `running`).
pub(crate) async fn maybe_start_run(pool: &PgPool, ingest_run_id: &str) -> Result<()> {
    let run_id: Uuid = ingest_run_id
        .parse()
        .context("invalid ingest_run_id UUID")?;

    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'running', started_at = COALESCE(started_at, now()) \
         WHERE id = $1 AND status = 'queued'",
    )
    .bind(run_id)
    .execute(pool)
    .await
    .context("failed to transition ingestion_run to running")?;

    Ok(())
}

/// If all pipeline stages have succeeded, mark the run `succeeded`.
pub(crate) async fn maybe_complete_run(pool: &PgPool, ingest_run_id: &str) -> Result<()> {
    let run_id: Uuid = ingest_run_id
        .parse()
        .context("invalid ingest_run_id UUID")?;

    let succeeded: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM control.pipeline_stage_runs \
         WHERE ingestion_run_id = $1 AND status = 'succeeded'",
    )
    .bind(run_id)
    .fetch_one(pool)
    .await
    .context("failed to count succeeded stages")?;

    if succeeded >= TOTAL_PIPELINE_STAGES {
        sqlx::query(
            "UPDATE control.ingestion_runs \
             SET status = 'succeeded', finished_at = now() \
             WHERE id = $1 AND status IN ('queued', 'running')",
        )
        .bind(run_id)
        .execute(pool)
        .await
        .context("failed to mark ingestion_run succeeded")?;
    }

    Ok(())
}

/// Mark `ingestion_runs` as `failed` on any stage failure.
pub(crate) async fn fail_run(
    pool: &PgPool,
    ingest_run_id: &str,
    error_message: &str,
) -> Result<()> {
    let run_id: Uuid = ingest_run_id
        .parse()
        .context("invalid ingest_run_id UUID")?;

    let error = if error_message.is_empty() {
        None
    } else {
        Some(error_message)
    };

    sqlx::query(
        "UPDATE control.ingestion_runs \
         SET status = 'failed', finished_at = now(), error = $2 \
         WHERE id = $1 AND status IN ('queued', 'running')",
    )
    .bind(run_id)
    .bind(error)
    .execute(pool)
    .await
    .context("failed to mark ingestion_run failed")?;

    Ok(())
}

/// Apply all DB updates for one [`IngestStatusEvent`].
pub(crate) async fn handle_db_updates(pool: &PgPool, ev: &IngestStatusEvent) -> Result<()> {
    let status = IngestStatus::try_from(ev.status).unwrap_or(IngestStatus::Unspecified);
    let stage_opt = IngestStage::try_from(ev.stage).ok().and_then(stage_label);

    if let Some(stage_str) = stage_opt {
        if let Some((db_status, error)) = stage_db_params(status, &ev.error_message) {
            update_stage_run(pool, &ev.ingest_run_id, stage_str, db_status, error).await?;
        }
    }

    match status {
        IngestStatus::Processing => {
            maybe_start_run(pool, &ev.ingest_run_id).await?;
        }
        IngestStatus::Done => {
            maybe_complete_run(pool, &ev.ingest_run_id).await?;
        }
        IngestStatus::Failed => {
            fail_run(pool, &ev.ingest_run_id, &ev.error_message).await?;
        }
        IngestStatus::Pending | IngestStatus::Unspecified => {}
    }

    Ok(())
}
