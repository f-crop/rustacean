/// Invoke `f` and catch any panic from within the redaction call.
///
/// On success returns `Some(redacted)`. On panic returns `None` and emits a
/// structured `tracing::error!` with `error_kind="redaction_failed"` — the
/// caller must drop the offending line (ADR-013 §6.3 fail-closed contract).
pub(super) fn redact_guarded<F>(f: F, session_id: &str) -> Option<String>
where
    F: FnOnce() -> String + std::panic::UnwindSafe,
{
    if let Ok(s) = std::panic::catch_unwind(f) {
        Some(s)
    } else {
        tracing::error!(
            session_id = %session_id,
            error_kind = "redaction_failed",
            "redact_with_token panicked; dropping line (ADR-013 §6.3 fail-closed)"
        );
        None
    }
}
