//! Daily summary report — one JSON file per UTC day.
//!
//! Written to `<state_dir>/<date>.json`. Pass/fail verdict per ADR-012 §2.7.2.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::state::HarnessState;

/// ADR-012 §2.7.2 pass/fail thresholds.
pub const AVAILABILITY_THRESHOLD_PCT: f64 = 99.5;
pub const INGESTION_SUCCESS_THRESHOLD_PCT: f64 = 99.0;
pub const AGENT_SUCCESS_THRESHOLD_PCT: f64 = 95.0;
pub const QUERY_P95_LATENCY_THRESHOLD_SECS: f64 = 2.0;
pub const KAFKA_LAG_THRESHOLD: u64 = 10_000;
pub const OUTBOX_AGE_THRESHOLD_SECS: f64 = 60.0;

/// Verdict for a single threshold check.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ThresholdCheck {
    pub name: String,
    pub threshold: String,
    pub actual: String,
    pub pass: bool,
}

/// Daily summary written to `<state_dir>/<date>.json`.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DailySummary {
    pub date: String,
    pub generated_at: DateTime<Utc>,
    pub run_number: u32,
    pub elapsed_hours: f64,

    pub ingestion_ok: u64,
    pub ingestion_err: u64,
    pub ingestion_success_pct: f64,

    pub agent_ok: u64,
    pub agent_err: u64,
    pub agent_success_pct: f64,

    pub query_ok: u64,
    pub query_err: u64,
    pub query_success_pct: f64,

    pub query_p95_latency_secs: Option<f64>,

    pub health_checks_total: u64,
    pub health_checks_failed: u64,
    pub availability_pct: f64,
    pub degradation_events: u64,
    pub drift_events: u64,

    pub last_failed_trace_id: Option<String>,

    pub threshold_checks: Vec<ThresholdCheck>,

    /// Overall pass/fail verdict for the Wave 8 exit gate.
    pub verdict: String,
}

impl DailySummary {
    pub fn from_state(state: &HarnessState) -> Self {
        let now = Utc::now();
        let date = now.format("%Y-%m-%d").to_string();
        let elapsed_hours = state.elapsed_secs() / 3600.0;

        let ingestion_success_pct = state.ingestion.success_rate();
        let agent_success_pct = state.agent.success_rate();
        let query_success_pct = state.query.success_rate();
        let availability_pct = state.health.availability_pct();
        let query_p95 = state.query_p95_seconds();

        let checks = build_threshold_checks(
            availability_pct,
            ingestion_success_pct,
            agent_success_pct,
            query_p95,
        );

        let verdict = if checks.iter().all(|c| c.pass) {
            "PASS".to_owned()
        } else {
            "FAIL".to_owned()
        };

        Self {
            date,
            generated_at: now,
            run_number: state.run_number,
            elapsed_hours,
            ingestion_ok: state.ingestion.iterations_ok,
            ingestion_err: state.ingestion.iterations_err,
            ingestion_success_pct,
            agent_ok: state.agent.iterations_ok,
            agent_err: state.agent.iterations_err,
            agent_success_pct,
            query_ok: state.query.iterations_ok,
            query_err: state.query.iterations_err,
            query_success_pct,
            query_p95_latency_secs: query_p95,
            health_checks_total: state.health.checks_total,
            health_checks_failed: state.health.checks_failed,
            availability_pct,
            degradation_events: state.health.degradation_events,
            drift_events: state.health.drift_events,
            last_failed_trace_id: state.last_failed_trace_id.clone(),
            threshold_checks: checks,
            verdict,
        }
    }

    /// Write to `<state_dir>/<date>.json`.
    pub fn write(&self, state_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
        let path = state_dir.join(format!("{}.json", self.date));
        let bytes = serde_json::to_vec_pretty(self).context("failed to serialize daily summary")?;
        std::fs::write(&path, &bytes)
            .with_context(|| format!("failed to write daily summary to {}", path.display()))?;
        tracing::info!(path = %path.display(), verdict = %self.verdict, "daily summary written");
        Ok(())
    }
}

fn build_threshold_checks(
    availability_pct: f64,
    ingestion_success_pct: f64,
    agent_success_pct: f64,
    query_p95: Option<f64>,
) -> Vec<ThresholdCheck> {
    let mut checks = vec![
        ThresholdCheck {
            name: "availability".to_owned(),
            threshold: format!("≥ {AVAILABILITY_THRESHOLD_PCT:.1}%"),
            actual: format!("{availability_pct:.2}%"),
            pass: availability_pct >= AVAILABILITY_THRESHOLD_PCT,
        },
        ThresholdCheck {
            name: "ingestion_success_rate".to_owned(),
            threshold: format!("≥ {INGESTION_SUCCESS_THRESHOLD_PCT:.1}%"),
            actual: format!("{ingestion_success_pct:.2}%"),
            pass: ingestion_success_pct >= INGESTION_SUCCESS_THRESHOLD_PCT,
        },
        ThresholdCheck {
            name: "agent_success_rate".to_owned(),
            threshold: format!("≥ {AGENT_SUCCESS_THRESHOLD_PCT:.1}%"),
            actual: format!("{agent_success_pct:.2}%"),
            pass: agent_success_pct >= AGENT_SUCCESS_THRESHOLD_PCT,
        },
    ];

    if let Some(p95) = query_p95 {
        checks.push(ThresholdCheck {
            name: "query_p95_latency".to_owned(),
            threshold: format!("≤ {QUERY_P95_LATENCY_THRESHOLD_SECS:.1} s"),
            actual: format!("{p95:.3} s"),
            pass: p95 <= QUERY_P95_LATENCY_THRESHOLD_SECS,
        });
    }

    checks
}
