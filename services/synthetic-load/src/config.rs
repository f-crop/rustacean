use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};

pub struct Config {
    /// Base URL of the control-api (e.g. `http://control-api:4000`).
    pub target_url: String,

    /// `RB_ADMIN_TOKEN` — used for admin force-delete + impersonation.
    pub admin_token: String,

    /// Postgres DSN for direct email-verification writes after signup.
    pub database_url: String,

    /// Number of active synthetic tenants to maintain simultaneously.
    pub tenant_count: usize,

    /// Directory where run state and daily summaries are persisted.
    pub state_dir: PathBuf,

    /// Total target soak duration; harness exits cleanly when this elapses.
    pub soak_duration: Duration,

    /// Prometheus HTTP API base URL (e.g. `http://prometheus:9090`).
    /// Used to query `rb_outbox_age_seconds` and `rb_kafka_consumer_lag`.
    pub prometheus_url: Option<String>,

    /// Comma-separated list of service base URLs to health-check.
    /// Defaults to just `target_url`.
    pub service_urls: Vec<String>,

    /// Interval between health check passes.
    pub health_interval: Duration,

    /// Interval between ingestion-loop iterations per tenant.
    pub ingestion_interval: Duration,

    /// Interval between agent-loop iterations per tenant.
    pub agent_interval: Duration,

    /// Interval between query-loop iterations per tenant.
    pub query_interval: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let target_url = std::env::var("SYNTHETIC_LOAD_TARGET")
            .context("SYNTHETIC_LOAD_TARGET is required (e.g. http://control-api:4000)")?;

        let admin_token = std::env::var("SYNTHETIC_LOAD_ADMIN_TOKEN")
            .context("SYNTHETIC_LOAD_ADMIN_TOKEN is required (same as RB_ADMIN_TOKEN)")?;

        let database_url = std::env::var("SYNTHETIC_LOAD_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .context("SYNTHETIC_LOAD_DATABASE_URL or DATABASE_URL is required")?;

        let tenant_count: usize = std::env::var("SYNTHETIC_LOAD_TENANT_COUNT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(10);

        let state_dir = std::env::var("SYNTHETIC_LOAD_STATE_DIR")
            .map_or_else(|_| dirs_or_home().join("synthetic-load"), PathBuf::from);

        let soak_days: u64 = std::env::var("SYNTHETIC_LOAD_DAYS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(7);
        let soak_duration = Duration::from_secs(soak_days * 24 * 3600);

        let prometheus_url = std::env::var("SYNTHETIC_LOAD_PROMETHEUS_URL").ok();

        let service_urls = std::env::var("SYNTHETIC_LOAD_SERVICE_URLS").map_or_else(
            |_| vec![target_url.clone()],
            |s| s.split(',').map(str::trim).map(String::from).collect(),
        );

        let health_interval = parse_duration_secs("SYNTHETIC_LOAD_HEALTH_INTERVAL_SECS", 60);
        let ingestion_interval = parse_duration_secs("SYNTHETIC_LOAD_INGEST_INTERVAL_SECS", 120);
        let agent_interval = parse_duration_secs("SYNTHETIC_LOAD_AGENT_INTERVAL_SECS", 90);
        let query_interval = parse_duration_secs("SYNTHETIC_LOAD_QUERY_INTERVAL_SECS", 30);

        if !target_url.starts_with("http") {
            anyhow::bail!("SYNTHETIC_LOAD_TARGET must be an http(s) URL, got: {target_url}");
        }

        Ok(Self {
            target_url,
            admin_token,
            database_url,
            tenant_count,
            state_dir,
            soak_duration,
            prometheus_url,
            service_urls,
            health_interval,
            ingestion_interval,
            agent_interval,
            query_interval,
        })
    }
}

fn dirs_or_home() -> PathBuf {
    // ~/.local/state/rustbrain/synthetic-load
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("rustbrain");
    }
    PathBuf::from("/var/lib/rustbrain")
}

fn parse_duration_secs(var: &str, default: u64) -> Duration {
    Duration::from_secs(
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(default),
    )
}
