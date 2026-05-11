//! Shared session-lifecycle constants and pure helpers.
//!
//! Extracted from `sessions.rs` and `events.rs` so both can reference a single
//! source of truth for status constants and avoid the 600-line file-size cap.

use rb_schemas::AgentRuntime;

use crate::error::AppError;

// ---------------------------------------------------------------------------
// Status constants
// ---------------------------------------------------------------------------

/// Statuses that agent-runner is allowed to set via the internal callback.
pub const VALID_AGENT_STATUSES: &[&str] = &[
    "pending",
    "running",
    "terminating",
    "terminated",
    "cancelled",
];

/// Statuses that are terminal — a session in one of these will not transition further.
pub const TERMINAL_STATUSES: &[&str] = &["terminated", "cancelled"];

/// Statuses considered "live" — an SSE stream should open for these.
pub const LIVE_STATUSES: &[&str] = &["pending", "running", "terminating"];

// ---------------------------------------------------------------------------
// Pure helpers
// ---------------------------------------------------------------------------

/// Maximum Unicode code points stored as a prompt preview in the DB.
const PROMPT_PREVIEW_MAX_CHARS: usize = 256;

/// Returns the first ≤`PROMPT_PREVIEW_MAX_CHARS` Unicode code points of `s`.
pub fn prompt_preview(s: &str) -> String {
    s.chars().take(PROMPT_PREVIEW_MAX_CHARS).collect()
}

pub fn parse_runtime(s: &str) -> Option<AgentRuntime> {
    match s {
        "claude_code" => Some(AgentRuntime::ClaudeCode),
        "opencode" => Some(AgentRuntime::Opencode),
        "pi" => Some(AgentRuntime::Pi),
        _ => None,
    }
}

/// Validate that `workspace_path` is a safe relative path (no `..`, no absolute).
/// Returns an error on invalid input so the session is never created.
pub fn validate_workspace_path(path: &str) -> Result<(), AppError> {
    let p = std::path::Path::new(path);
    if p.is_absolute() {
        return Err(AppError::InvalidInput);
    }
    for component in p.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir | Component::CurDir) {
            return Err(AppError::InvalidInput);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_preview_short_string_unchanged() {
        assert_eq!(prompt_preview("Hello, world!"), "Hello, world!");
    }

    #[test]
    fn prompt_preview_truncates_at_256_chars() {
        let s: String = "x".repeat(1000);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
    }

    #[test]
    fn prompt_preview_handles_multibyte_unicode() {
        let s: String = "🦀".repeat(300);
        let preview = prompt_preview(&s);
        assert_eq!(preview.chars().count(), PROMPT_PREVIEW_MAX_CHARS);
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn parse_runtime_valid_values() {
        assert_eq!(parse_runtime("claude_code"), Some(AgentRuntime::ClaudeCode));
        assert_eq!(parse_runtime("opencode"), Some(AgentRuntime::Opencode));
        assert_eq!(parse_runtime("pi"), Some(AgentRuntime::Pi));
    }

    #[test]
    fn parse_runtime_invalid_returns_none() {
        assert_eq!(parse_runtime("unknown"), None);
        assert_eq!(parse_runtime(""), None);
    }

    #[test]
    fn validate_workspace_path_rejects_traversal() {
        assert!(validate_workspace_path("../etc/passwd").is_err());
        assert!(validate_workspace_path("/absolute/path").is_err());
        assert!(validate_workspace_path("a/../../b").is_err());
        assert!(validate_workspace_path("./relative").is_err());
    }

    #[test]
    fn validate_workspace_path_accepts_valid_paths() {
        assert!(validate_workspace_path("tenant/session").is_ok());
        assert!(validate_workspace_path("abc123").is_ok());
        assert!(validate_workspace_path("tenant-id/session-id").is_ok());
    }

    #[test]
    fn valid_agent_statuses_includes_expected() {
        assert!(VALID_AGENT_STATUSES.contains(&"pending"));
        assert!(VALID_AGENT_STATUSES.contains(&"running"));
        assert!(VALID_AGENT_STATUSES.contains(&"terminating"));
        assert!(VALID_AGENT_STATUSES.contains(&"terminated"));
        assert!(VALID_AGENT_STATUSES.contains(&"cancelled"));
        assert!(!VALID_AGENT_STATUSES.contains(&"unknown"));
        assert!(!VALID_AGENT_STATUSES.contains(&"'DROP TABLE'"));
    }

    #[test]
    fn terminal_statuses_are_subset_of_valid() {
        for ts in TERMINAL_STATUSES {
            assert!(
                VALID_AGENT_STATUSES.contains(ts),
                "TERMINAL_STATUSES entry '{ts}' must also appear in VALID_AGENT_STATUSES"
            );
        }
    }

    #[test]
    fn live_statuses_includes_expected() {
        assert!(LIVE_STATUSES.contains(&"pending"));
        assert!(LIVE_STATUSES.contains(&"running"));
        assert!(LIVE_STATUSES.contains(&"terminating"));
        assert!(!LIVE_STATUSES.contains(&"terminated"));
        assert!(!LIVE_STATUSES.contains(&"cancelled"));
    }

    #[test]
    fn live_and_terminal_are_disjoint() {
        for s in LIVE_STATUSES {
            assert!(
                !TERMINAL_STATUSES.contains(s),
                "status '{s}' appears in both LIVE_STATUSES and TERMINAL_STATUSES"
            );
        }
    }
}
