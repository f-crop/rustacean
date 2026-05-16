use std::time::Duration;

/// Configuration for the `HealthyConsumer` watchdog.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How long with no successful messages received before the consumer is
    /// checked for a wedge.  Errors do NOT reset this clock.  Default: 60 s.
    pub stall_timeout: Duration,

    /// Maximum number of consecutive errors (from `next()`) before the
    /// consumer is force-recreated regardless of lag.  This detects the
    /// "consumer group disappeared while binary is up" failure mode where
    /// `lag_estimate()` returns 0 even though the consumer is stuck.
    /// Default: 30 (≈ 30 s at 1 s back-off per error in caller loops).
    pub max_error_streak: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stall_timeout: Duration::from_secs(60),
            max_error_streak: 30,
        }
    }
}
