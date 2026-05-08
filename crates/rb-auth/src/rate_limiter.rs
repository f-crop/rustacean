use std::time::{Duration, Instant};

use dashmap::DashMap;

use crate::error::AuthError;

const MAX_FAILURES: usize = 5;
const WINDOW_SECS: u64 = 600; // 10 minutes
const LOCKOUT_SECS: u64 = 900; // 15 minutes

/// In-memory sliding-window rate limiter for login attempts.
///
/// Tracks failed attempts per email address. After [`MAX_FAILURES`] failures
/// in a [`WINDOW_SECS`]-second window, the address is locked out for
/// [`LOCKOUT_SECS`] seconds.
///
/// Suitable for single-instance deployments. Multi-instance setups would
/// need a Redis-backed implementation behind the same interface.
#[derive(Default)]
pub struct LoginRateLimiter {
    failures: DashMap<String, Vec<Instant>>,
}

impl LoginRateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Check whether an email address is currently rate-limited.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::RateLimited`] with the remaining lockout duration
    /// if the address has exceeded the failure threshold.
    pub fn check(&self, email: &str) -> Result<(), AuthError> {
        let now = Instant::now();
        let window = Duration::from_secs(WINDOW_SECS);

        if let Some(attempts) = self.failures.get(email) {
            let recent: Vec<Instant> = attempts
                .iter()
                .copied()
                .filter(|t| now.duration_since(*t) < window)
                .collect();

            if recent.len() >= MAX_FAILURES {
                let oldest = recent.iter().copied().min().unwrap_or(now);
                let elapsed = now.duration_since(oldest).as_secs();
                let retry_after = LOCKOUT_SECS.saturating_sub(elapsed);
                return Err(AuthError::RateLimited {
                    retry_after_secs: retry_after.max(1),
                });
            }
        }
        Ok(())
    }

    /// Record a login attempt result for the given email.
    ///
    /// Successful attempts clear the failure history.
    /// Failed attempts are appended to the sliding window.
    pub fn record_attempt(&self, email: &str, success: bool) {
        if success {
            self.failures.remove(email);
        } else {
            let now = Instant::now();
            let window = Duration::from_secs(WINDOW_SECS);
            let mut entry = self.failures.entry(email.to_string()).or_default();
            entry.retain(|t| now.duration_since(*t) < window);
            entry.push(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn under_threshold_is_allowed() {
        let rl = LoginRateLimiter::new();
        for _ in 0..4 {
            rl.record_attempt("user@example.com", false);
        }
        assert!(rl.check("user@example.com").is_ok());
    }

    #[test]
    fn at_threshold_is_blocked() {
        let rl = LoginRateLimiter::new();
        for _ in 0..MAX_FAILURES {
            rl.record_attempt("user@example.com", false);
        }
        assert!(matches!(
            rl.check("user@example.com"),
            Err(AuthError::RateLimited { .. })
        ));
    }

    #[test]
    fn success_clears_failures() {
        let rl = LoginRateLimiter::new();
        for _ in 0..MAX_FAILURES {
            rl.record_attempt("user@example.com", false);
        }
        rl.record_attempt("user@example.com", true);
        assert!(rl.check("user@example.com").is_ok());
    }

    #[test]
    fn unknown_email_is_allowed() {
        let rl = LoginRateLimiter::new();
        assert!(rl.check("unknown@example.com").is_ok());
    }
}
