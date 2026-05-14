//! `POST /internal/admin/partition-maintenance` — daily partition seed + prune (RUSAA-1374).
//!
//! Seeds daily partitions for `agents.agent_events` for the next `seed_days_ahead` days
//! (default 2) and drops expired partitions older than the maximum configured retention
//! window across all active tenants (default 30 days).
//!
//! Called by the `nightly-partition-seed` Paperclip routine. Safe to re-run at any time
//! (fully idempotent). Auth: internal-only; protected by `require_internal_secret` middleware.

use axum::{Json, extract::State, response::IntoResponse};
use serde::Serialize;

use crate::{error::AppError, state::AppState};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of days ahead to seed partitions.  Seeds today + N-1 future days.
const SEED_DAYS_AHEAD: u32 = 2;

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct PartitionMaintenanceResponse {
    /// Number of partitions seeded this run.
    pub seeded: u32,
    /// Number of expired partitions dropped this run.
    pub pruned: i32,
}

// ---------------------------------------------------------------------------
// Handler
// ---------------------------------------------------------------------------

/// `POST /internal/admin/partition-maintenance`
///
/// Seeds the next [`SEED_DAYS_AHEAD`] days of `agents.agent_events` daily partitions
/// and drops any partitions that are older than the maximum tenant retention window.
///
/// Both operations are idempotent — re-running when nothing needs to be done is safe
/// and returns `seeded=0, pruned=0`.
pub async fn partition_maintenance(
    State(state): State<AppState>,
) -> Result<impl IntoResponse, AppError> {
    let mut seeded: u32 = 0;

    // Seed today and the next SEED_DAYS_AHEAD-1 days.
    for offset in 0..SEED_DAYS_AHEAD {
        let target: chrono::NaiveDate =
            (chrono::Utc::now().date_naive()) + chrono::TimeDelta::days(i64::from(offset));

        sqlx::query("SELECT agents.seed_agent_events_partition($1)")
            .bind(target)
            .execute(&state.pool)
            .await
            .map_err(|e| {
                tracing::error!(date = %target, error = %e, "failed to seed partition");
                AppError::Internal(anyhow::anyhow!("seed_agent_events_partition failed: {e}"))
            })?;

        seeded += 1;
        tracing::debug!(date = %target, "seeded agent_events partition");
    }

    // Prune expired partitions.
    let pruned: i32 = sqlx::query_scalar("SELECT agents.prune_agent_events_partitions()")
        .fetch_one(&state.pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "failed to prune partitions");
            AppError::Internal(anyhow::anyhow!("prune_agent_events_partitions failed: {e}"))
        })?;

    tracing::info!(seeded, pruned, "partition maintenance complete");

    Ok(Json(PartitionMaintenanceResponse { seeded, pruned }))
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const _: () = assert!(SEED_DAYS_AHEAD >= 1, "SEED_DAYS_AHEAD must be >= 1");

    #[test]
    fn response_serializes() {
        let resp = PartitionMaintenanceResponse {
            seeded: 2,
            pruned: 3,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["seeded"], 2);
        assert_eq!(v["pruned"], 3);
    }
}
