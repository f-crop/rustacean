//! Compile-time build provenance for rust-brain services.
//!
//! Values are captured at compile time from environment variables set by the
//! Docker builder stage (`RB_BUILD_SHA`, `RB_BUILD_TIMESTAMP`, `RB_BUILD_DIRTY`)
//! or by `services/<name>/build.rs` for local (non-Docker) builds.
//! All fields fall back to `"unknown"` when the variable is absent.

use serde::Serialize;

/// Compile-time build provenance baked into the binary.
#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    pub sha: &'static str,
    pub timestamp: &'static str,
    pub dirty: &'static str,
}

pub const SHA: &str = if let Some(s) = option_env!("RB_BUILD_SHA") {
    s
} else {
    "unknown"
};

pub const TIMESTAMP: &str = if let Some(s) = option_env!("RB_BUILD_TIMESTAMP") {
    s
} else {
    "unknown"
};

pub const DIRTY: &str = if let Some(s) = option_env!("RB_BUILD_DIRTY") {
    s
} else {
    "false"
};

/// Returns compile-time build provenance.
#[must_use]
pub fn get() -> BuildInfo {
    BuildInfo {
        sha: SHA,
        timestamp: TIMESTAMP,
        dirty: DIRTY,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_returns_static_fields() {
        let info = get();
        // SHA and TIMESTAMP fall back to "unknown" in unit test builds
        // where RB_BUILD_SHA/RB_BUILD_TIMESTAMP are not set.
        assert!(!info.sha.is_empty());
        assert!(!info.timestamp.is_empty());
        assert!(!info.dirty.is_empty());
    }

    #[test]
    fn dirty_default_is_false_string() {
        // Ensures callers can check `info.dirty == "false"` as a boolean-ish string.
        assert!(info_dirty_is_bool_string(DIRTY));
    }

    fn info_dirty_is_bool_string(s: &str) -> bool {
        matches!(s, "true" | "false")
    }
}
