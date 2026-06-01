//! Persisted harness state — checkpoint and resume semantics.
//!
//! State is written to `<state_dir>/harness-state.json` on every checkpoint.
//! On restart, the harness loads this file and resumes. If the gap between
//! the last checkpoint and now exceeds 1 hour, the soak clock resets to zero
//! (ADR-012 §2.7.2 resume semantics).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::tenant::TenantRecord;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "harness-state.json";
const RESUME_GAP_LIMIT: Duration = Duration::from_secs(3600);

/// Per-loop iteration counters.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct LoopCounters {
    pub iterations_ok: u64,
    pub iterations_err: u64,
}

impl LoopCounters {
    pub fn record(&mut self, ok: bool) {
        if ok {
            self.iterations_ok += 1;
        } else {
            self.iterations_err += 1;
        }
    }

    pub fn total(&self) -> u64 {
        self.iterations_ok + self.iterations_err
    }

    pub fn success_rate(&self) -> f64 {
        let t = self.total();
        if t == 0 {
            return 100.0;
        }
        #[allow(clippy::cast_precision_loss)]
        let result = (self.iterations_ok as f64 / t as f64) * 100.0;
        result
    }
}

/// Accumulated health-gate state over the run.
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct HealthCounters {
    pub checks_total: u64,
    pub checks_failed: u64,
    pub degradation_events: u64,
    pub drift_events: u64,
}

impl HealthCounters {
    pub fn availability_pct(&self) -> f64 {
        if self.checks_total == 0 {
            return 100.0;
        }
        let passed = self.checks_total.saturating_sub(self.checks_failed);
        #[allow(clippy::cast_precision_loss)]
        let result = (passed as f64 / self.checks_total as f64) * 100.0;
        result
    }
}

/// Top-level persisted harness state.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HarnessState {
    /// Wall-clock time when the soak effectively started (after any resume).
    pub effective_start: DateTime<Utc>,

    /// Time of the last checkpoint write.
    pub last_checkpoint: DateTime<Utc>,

    /// Run number — incremented each time the soak clock is reset.
    pub run_number: u32,

    /// Provisioned tenant pool.
    pub tenants: Vec<TenantRecord>,

    /// Next slot number for new tenant provisioning.
    pub next_slot: usize,

    /// Loop-specific counters.
    pub ingestion: LoopCounters,
    pub agent: LoopCounters,
    pub query: LoopCounters,

    /// Health gate counters.
    pub health: HealthCounters,

    /// SHA captured at harness start (from the first health/build response).
    pub expected_sha: Option<String>,

    /// Trace ID of the most recent failed request across any loop.
    pub last_failed_trace_id: Option<String>,

    /// Rolling window of the last 1000 query latency samples for p95 approximation.
    pub query_latency_samples_ms: VecDeque<u64>,
}

impl HarnessState {
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            effective_start: now,
            last_checkpoint: now,
            run_number: 1,
            tenants: Vec::new(),
            next_slot: 0,
            ingestion: LoopCounters::default(),
            agent: LoopCounters::default(),
            query: LoopCounters::default(),
            health: HealthCounters::default(),
            expected_sha: None,
            last_failed_trace_id: None,
            query_latency_samples_ms: VecDeque::new(),
        }
    }

    /// Seconds elapsed since `effective_start`.
    pub fn elapsed_secs(&self) -> f64 {
        #[allow(clippy::cast_precision_loss)]
        let secs = (Utc::now() - self.effective_start).num_seconds() as f64;
        secs
    }

    /// Append a query latency sample; keep the window at ≤1000 entries.
    pub fn record_query_latency(&mut self, latency_ms: u64) {
        if self.query_latency_samples_ms.len() >= 1000 {
            self.query_latency_samples_ms.pop_front();
        }
        self.query_latency_samples_ms.push_back(latency_ms);
    }

    /// Approximate p95 query latency in seconds from the rolling window.
    pub fn query_p95_seconds(&self) -> Option<f64> {
        if self.query_latency_samples_ms.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = self.query_latency_samples_ms.iter().copied().collect();
        sorted.sort_unstable();
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )]
        let idx = (sorted.len() as f64 * 0.95) as usize;
        let idx = idx.min(sorted.len() - 1);
        #[allow(clippy::cast_precision_loss)]
        Some(sorted[idx] as f64 / 1000.0)
    }

    /// Path to the state file.
    pub fn state_file(state_dir: &Path) -> PathBuf {
        state_dir.join(STATE_FILE)
    }

    /// Load from disk, applying resume semantics. Returns a fresh state if the
    /// file does not exist.
    pub fn load_or_new(state_dir: &Path) -> Result<Self> {
        let path = Self::state_file(state_dir);
        if !path.exists() {
            tracing::info!("no prior state file; starting fresh soak");
            return Ok(Self::new());
        }

        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read state file {}", path.display()))?;
        let mut state: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("failed to parse state file {}", path.display()))?;

        let gap = Utc::now() - state.last_checkpoint;
        let gap_duration = Duration::from_secs(gap.num_seconds().unsigned_abs());

        if gap_duration > RESUME_GAP_LIMIT {
            tracing::warn!(
                gap_secs = gap.num_seconds(),
                "gap exceeds 1 h — resetting soak clock"
            );
            state.effective_start = Utc::now();
            state.run_number += 1;
        } else {
            tracing::info!(
                gap_secs = gap.num_seconds(),
                run_number = state.run_number,
                "resuming harness; gap within 1 h threshold"
            );
        }
        state.last_checkpoint = Utc::now();
        Ok(state)
    }

    /// Persist to disk atomically (write temp then rename).
    pub fn save(&mut self, state_dir: &Path) -> Result<()> {
        self.last_checkpoint = Utc::now();
        let path = Self::state_file(state_dir);
        std::fs::create_dir_all(state_dir)
            .with_context(|| format!("failed to create state dir {}", state_dir.display()))?;
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).context("failed to serialize state")?;
        std::fs::write(&tmp, &bytes)
            .with_context(|| format!("failed to write temp state file {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("failed to rename state file to {}", path.display()))?;
        Ok(())
    }
}

impl Default for HarnessState {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of one loop iteration.
#[derive(Debug, Clone)]
pub struct IterationOutcome {
    pub loop_name: &'static str,
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub trace_id: Option<String>,
}
