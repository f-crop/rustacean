//! Log redaction for runtime transcripts (ADR-013 §6.3).
//!
//! `redact` / `redact_with_token` are applied to every runtime stdout byte
//! before it reaches `chat_messages.body`, `agent_events.data`, an SSE
//! frame, or a structured log line. Redaction is fail-closed: if this
//! panics the caller must drop the line and emit `error_kind="redaction_failed"`.
//!
//! Patterns redacted:
//! 1. JWTs — three base64url segments starting with `eyJ`.
//! 2. `Bearer <token>` (case-insensitive prefix).
//! 3. `rb_live_<hex>` API key literals.
//! 4. Env-var names `RB_MCP_JWT_SECRET`, `RB_AGENT_API_KEY`.
//! 5. The exact live-session JWT, if supplied by the caller.

use std::borrow::Cow;

const REDACTED_JWT: &str = "<redacted:jwt>";
const REDACTED_BEARER: &str = "<redacted:bearer>";
const REDACTED_SECRET: &str = "<redacted:secret>";

/// Apply the standard deny-list to a log line.
///
/// Returns `Cow::Borrowed` when nothing matches (no allocation on the hot path).
#[must_use]
pub fn redact(s: &str) -> Cow<'_, str> {
    redact_with_token(s, None)
}

/// Apply the deny-list plus an optional exact live-token value.
///
/// `live_token` is the full JWT currently in the runtime's `.mcp.json`.
/// Passing it here catches verbatim echoes even if the regex-like patterns
/// would miss a split or encoded form.
#[must_use]
pub fn redact_with_token<'a>(s: &'a str, live_token: Option<&str>) -> Cow<'a, str> {
    if !needs_scan(s) && live_token.is_none_or(|t| !s.contains(t)) {
        return Cow::Borrowed(s);
    }

    let mut buf = s.to_owned();
    apply_live_token(&mut buf, live_token);
    apply_jwt_pattern(&mut buf);
    apply_bearer_pattern(&mut buf);
    apply_prefix_pattern(&mut buf);
    Cow::Owned(buf)
}

fn needs_scan(s: &str) -> bool {
    s.contains("eyJ")
        || s.contains("rb_live_")
        || s.to_ascii_lowercase().contains("bearer ")
        || s.contains("RB_MCP_JWT_SECRET")
        || s.contains("RB_AGENT_API_KEY")
}

fn apply_live_token(buf: &mut String, live_token: Option<&str>) {
    if let Some(t) = live_token {
        if !t.is_empty() && buf.contains(t) {
            *buf = buf.replace(t, REDACTED_JWT);
        }
    }
}

/// Redact JWT patterns: `eyJ<b64url>.<b64url>.<b64url>`.
fn apply_jwt_pattern(buf: &mut String) {
    if !buf.contains("eyJ") {
        return;
    }
    let mut out = String::with_capacity(buf.len());
    let mut rest = buf.as_str();
    while let Some(pos) = rest.find("eyJ") {
        out.push_str(&rest[..pos]);
        let candidate = &rest[pos..];
        if let Some(end) = jwt_span(candidate) {
            out.push_str(REDACTED_JWT);
            rest = &candidate[end..];
        } else {
            out.push_str("eyJ");
            rest = &candidate[3..];
        }
    }
    out.push_str(rest);
    if out != *buf {
        *buf = out;
    }
}

/// Returns the byte length of a JWT at the start of `s`, or `None`.
fn jwt_span(s: &str) -> Option<usize> {
    let dot1 = s.find('.')?;
    if !is_b64url(&s[..dot1]) {
        return None;
    }
    let after1 = &s[dot1 + 1..];
    let dot2 = after1.find('.')?;
    if !is_b64url(&after1[..dot2]) {
        return None;
    }
    let sig = &after1[dot2 + 1..];
    let sig_len = sig.bytes().take_while(|&b| is_b64url_byte(b)).count();
    if sig_len == 0 {
        return None;
    }
    Some(dot1 + 1 + dot2 + 1 + sig_len)
}

fn is_b64url(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_b64url_byte)
}

fn is_b64url_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'='
}

/// Redact `Bearer <token>` (case-insensitive).
fn apply_bearer_pattern(buf: &mut String) {
    let lower = buf.to_ascii_lowercase();
    if !lower.contains("bearer ") {
        return;
    }
    // Find first occurrence; replace just the token part after "Bearer ".
    if let Some(pos) = lower.find("bearer ") {
        let token_start = pos + 7;
        let token_end = buf[token_start..]
            .bytes()
            .take_while(|&b| !b.is_ascii_whitespace())
            .count()
            + token_start;
        let prefix = buf[..pos + 7].to_owned();
        let suffix = buf[token_end..].to_owned();
        *buf = format!("{prefix}{REDACTED_BEARER}{suffix}");
    }
}

/// Redact `rb_live_<hex>` and known env-var names.
fn apply_prefix_pattern(buf: &mut String) {
    if buf.contains("rb_live_") {
        let mut out = String::with_capacity(buf.len());
        let mut rest = buf.as_str();
        while let Some(pos) = rest.find("rb_live_") {
            out.push_str(&rest[..pos]);
            let after = &rest[pos + 8..];
            let key_len = after.bytes().take_while(|&b| b.is_ascii_hexdigit()).count();
            out.push_str(REDACTED_SECRET);
            rest = &after[key_len..];
        }
        out.push_str(rest);
        if out != *buf {
            *buf = out;
        }
    }
    for name in ["RB_MCP_JWT_SECRET", "RB_AGENT_API_KEY"] {
        if buf.contains(name) {
            *buf = buf.replace(name, REDACTED_SECRET);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_line_is_returned_borrowed() {
        let s = "hello world";
        assert!(matches!(redact(s), Cow::Borrowed(_)));
    }

    #[test]
    fn jwt_in_line_is_redacted() {
        // Build a synthetic three-part JWT-shaped string.
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let line = format!("MCP config token={jwt} loaded");
        let out = redact(&line);
        assert!(!out.contains("eyJ"), "JWT must be redacted");
        assert!(out.contains(REDACTED_JWT));
    }

    #[test]
    fn bearer_token_is_redacted() {
        let line = "Authorization: Bearer mysecrettoken123";
        let out = redact(line);
        assert!(!out.contains("mysecrettoken123"));
        assert!(out.contains(REDACTED_BEARER));
    }

    #[test]
    fn bearer_is_case_insensitive() {
        let line = "authorization: BEARER mysecrettoken456";
        let out = redact(line);
        assert!(!out.contains("mysecrettoken456"));
    }

    #[test]
    fn api_key_prefix_is_redacted() {
        let line = "key=rb_live_deadbeef1234abcd";
        let out = redact(line);
        assert!(!out.contains("rb_live_"));
        assert!(out.contains(REDACTED_SECRET));
    }

    #[test]
    fn env_var_names_are_redacted() {
        let line = "env: RB_MCP_JWT_SECRET=abc RB_AGENT_API_KEY=xyz";
        let out = redact(line);
        assert!(!out.contains("RB_MCP_JWT_SECRET"));
        assert!(!out.contains("RB_AGENT_API_KEY"));
    }

    #[test]
    fn live_token_verbatim_echo_is_caught() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let line = format!("model says: use token {jwt} for auth");
        let out = redact_with_token(&line, Some(jwt));
        assert!(!out.contains(jwt), "verbatim token must be stripped");
        assert!(out.contains(REDACTED_JWT));
    }

    #[test]
    fn redaction_negative_jwt_in_stdout_absent_from_captured_log() {
        // Regression test: a JWT injected into stdout must not appear
        // in the captured log sink after redact_with_token is applied.
        let live_jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzZXNzaW9uIn0.abc123def456ghi789";
        let stdout_line = format!("runtime echoed token={live_jwt}");

        // Simulate the stdio bridge: capture to a log sink via redact_with_token.
        let captured = redact_with_token(&stdout_line, Some(live_jwt));

        // The JWT must not appear in anything that would be logged or persisted.
        assert!(
            !captured.contains(live_jwt),
            "JWT must be absent from captured log: {captured}"
        );
        assert!(
            !captured.contains("eyJ"),
            "JWT prefix must be absent from captured log: {captured}"
        );
    }
}
