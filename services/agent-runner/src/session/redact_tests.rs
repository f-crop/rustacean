//! Tests for ADR-013 §6.3: `catch_unwind` fail-closed contract for `redact_guarded`.

/// Verify that a panicking redact closure is caught and `None` returned.
/// This proves the fail-closed contract: a redaction panic must never
/// propagate to the stdio-handler task and crash the session loop.
#[test]
fn redact_guarded_returns_none_and_does_not_propagate_panic() {
    let result = super::redact::redact_guarded(
        || panic!("deliberate test-injected redaction panic"),
        "test-session-id",
    );
    assert!(
        result.is_none(),
        "panicking redact must be caught and return None (line must be dropped)"
    );
}

/// Verify that a non-panicking redact closure passes through its result.
#[test]
fn redact_guarded_returns_redacted_string_on_success() {
    let result =
        super::redact::redact_guarded(|| String::from("clean output line"), "test-session-id");
    assert_eq!(
        result.as_deref(),
        Some("clean output line"),
        "successful redact must return Some(result)"
    );
}

/// Verify that real redaction (JWT stripping) works through the guard.
#[test]
fn redact_guarded_strips_jwt_through_real_redact_call() {
    let jwt =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"; // gitleaks:allow
    let live_token = jwt.to_owned();
    let line = format!("token={jwt}");
    let result = super::redact::redact_guarded(
        std::panic::AssertUnwindSafe(|| {
            rb_secrets::redact_with_token(&line, Some(&live_token)).into_owned()
        }),
        "test-session-id",
    );
    let redacted = result.expect("successful redact must return Some");
    assert!(
        !redacted.contains(jwt),
        "JWT must be stripped by real redact_with_token through the guard"
    );
}
