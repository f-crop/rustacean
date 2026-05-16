use std::time::Duration;

/// Configuration for the `HealthyConsumer` watchdog.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How long with no messages received (and lag > 0) before the consumer
    /// is deemed wedged and recreated.  Default: 60 seconds.
    pub stall_timeout: Duration,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            stall_timeout: Duration::from_secs(60),
        }
    }
}
