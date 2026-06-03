//! Log redaction for runtime transcripts (ADR-013 §6.3).
//!
//! `redact` / `redact_with_token` are applied to every runtime stdout byte
//! before it reaches `chat_messages.body`, `agent_events.data`, an SSE
//! frame, or a structured log line. Redaction is fail-closed: if this
//! panics the caller must drop the line and emit `error_kind="redaction_failed"`.
//!
//! Patterns redacted (applied after percent-decoding the input):
//! 1. JWTs — three base64url segments starting with `eyJ`.
//! 2. `Bearer <token>` (case-insensitive prefix).
//! 3. `rb_live_<hex>` API key literals.
//! 4. Env-var names `RB_MCP_JWT_SECRET`, `RB_AGENT_API_KEY`, `RB_LLM_API_KEY`.
//! 5. The exact live-session JWT, if supplied by the caller.
//!
//! **Known residual risk (ADR-013 §6.3):** Multi-line and partial JWTs
//! (`header.payload` only, no signature segment; or tokens split across
//! newlines by runtime line-wrapping) are NOT caught by the three-segment
//! regex. Compensating control: the MCP JWT TTL is 15 minutes
//! (`RB_MCP_JWT_TTL_SECS`), tokens are read-only, and scope is
//! tenant-bound (ADR-013 §5.2/§6.3) — limiting the exfiltration window
//! even if a partial token leaks.

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
///
/// Input is percent-decoded before matching so that JWTs with `%2E`-encoded
/// dots (e.g. in URL query strings) are caught by the three-segment regex.
/// The returned string is the decoded+redacted form when decoding was needed.
#[must_use]
pub fn redact_with_token<'a>(s: &'a str, live_token: Option<&str>) -> Cow<'a, str> {
    // Percent-decode first so %2E-encoded dots don't evade the JWT pattern.
    let decoded: Option<String> = if s.contains('%') { percent_decode_ascii(s) } else { None };
    let working: &str = decoded.as_deref().unwrap_or(s);

    if !needs_scan(working) && live_token.is_none_or(|t| t.is_empty() || !working.contains(t)) {
        return Cow::Borrowed(s);
    }

    let mut buf = working.to_owned();
    apply_live_token(&mut buf, live_token);
    apply_jwt_pattern(&mut buf);
    apply_bearer_pattern(&mut buf);
    apply_prefix_pattern(&mut buf);
    Cow::Owned(buf)
}

/// Percent-decode ASCII-range sequences (`%HH` where decoded byte < 128).
///
/// Non-ASCII sequences (decoded byte ≥ 128) are left encoded to avoid
/// mangling multi-byte UTF-8. Returns `None` when no sequence was decoded
/// (fast path: no allocation when the input has no `%XX` ASCII sequences).
fn percent_decode_ascii(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let mut changed = false;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                let decoded = (hi << 4) | lo;
                if decoded < 128 {
                    out.push(decoded);
                    changed = true;
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    if changed {
        // SAFETY: input is valid UTF-8; we only insert ASCII bytes (< 128)
        // and copy all other bytes unchanged, preserving UTF-8 validity.
        Some(unsafe { String::from_utf8_unchecked(out) })
    } else {
        None
    }
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn needs_scan(s: &str) -> bool {
    s.contains("eyJ")
        || s.contains("rb_live_")
        || s.to_ascii_lowercase().contains("bearer ")
        || s.contains("RB_MCP_JWT_SECRET")
        || s.contains("RB_AGENT_API_KEY")
        || s.contains("RB_LLM_API_KEY")
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

/// Redact all `Bearer <token>` occurrences (case-insensitive) in a line.
fn apply_bearer_pattern(buf: &mut String) {
    let lower = buf.to_ascii_lowercase();
    if !lower.contains("bearer ") {
        return;
    }
    // Loop to catch multiple occurrences per line.
    let mut out = String::with_capacity(buf.len());
    let mut rest_orig = buf.as_str();
    let mut rest_lower = lower.as_str();
    while let Some(pos) = rest_lower.find("bearer ") {
        out.push_str(&rest_orig[..pos + 7]);
        let after = &rest_orig[pos + 7..];
        let tok_len = after
            .bytes()
            .take_while(|&b| !b.is_ascii_whitespace())
            .count();
        out.push_str(REDACTED_BEARER);
        rest_orig = &after[tok_len..];
        rest_lower = &rest_lower[pos + 7 + tok_len..];
    }
    out.push_str(rest_orig);
    if out != *buf {
        *buf = out;
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
    for name in ["RB_MCP_JWT_SECRET", "RB_AGENT_API_KEY", "RB_LLM_API_KEY"] {
        if buf.contains(name) {
            *buf = buf.replace(name, REDACTED_SECRET);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_url_encoded_jwt() {
        // JWT dots encoded as %2E — the three-segment regex must still fire
        // after percent-decoding. This is the MEDIUM-2 evasion vector.
        let input =
            "GET /v1/auth?token=eyJhbGciOiJIUzI1NiJ9%2EeyJzdWIiOiJ0ZXN0In0%2Esignature_here_12345";
        let out = redact(input);
        assert!(
            out.contains(REDACTED_JWT),
            "URL-encoded JWT must be redacted: {out}"
        );
        assert!(
            !out.contains("eyJhbGciOiJIUzI1NiJ9"),
            "raw JWT header must be absent: {out}"
        );
    }

    #[test]
    fn clean_line_is_returned_borrowed() {
        let s = "hello world";
        assert!(matches!(redact(s), Cow::Borrowed(_)));
    }

    #[test]
    fn jwt_in_line_is_redacted() {
        // Build a synthetic three-part JWT-shaped string.
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"; // gitleaks:allow
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
        let line = "env: RB_MCP_JWT_SECRET=abc RB_AGENT_API_KEY=xyz RB_LLM_API_KEY=sk-test";
        let out = redact(line);
        assert!(!out.contains("RB_MCP_JWT_SECRET"));
        assert!(!out.contains("RB_AGENT_API_KEY"));
        assert!(!out.contains("RB_LLM_API_KEY"));
    }

    #[test]
    fn bearer_multiple_occurrences_all_redacted() {
        let line = "a=Bearer tokA and b=Bearer tokB end";
        let out = redact(line);
        assert!(!out.contains("tokA"), "first token must be redacted");
        assert!(!out.contains("tokB"), "second token must be redacted");
        assert_eq!(out.matches(REDACTED_BEARER).count(), 2);
    }

    #[test]
    fn live_token_verbatim_echo_is_caught() {
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"; // gitleaks:allow
        let line = format!("model says: use token {jwt} for auth");
        let out = redact_with_token(&line, Some(jwt));
        assert!(!out.contains(jwt), "verbatim token must be stripped");
        assert!(out.contains(REDACTED_JWT));
    }

    #[test]
    fn redaction_negative_jwt_in_stdout_absent_from_captured_log() {
        // Regression test: a JWT injected into stdout must not appear
        // in the captured log sink after redact_with_token is applied.
        let live_jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJzZXNzaW9uIn0.abc123def456ghi789"; // gitleaks:allow
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

    // ── Fail-closed contract (§6.3): no panic on any valid UTF-8 input ──────

    #[test]
    fn no_panic_on_empty_input() {
        let out = redact("");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn no_panic_on_all_ascii_printable() {
        let printable: String = (0x20u8..=0x7eu8).map(|b| b as char).collect();
        let _ = redact(&printable);
    }

    #[test]
    fn no_panic_on_multi_byte_unicode() {
        let unicode = "こんにちは世界 🔑 café naïve résumé";
        let _ = redact(unicode);
    }

    #[test]
    fn no_panic_on_large_input() {
        let large = "x".repeat(1_000_000);
        let out = redact(&large);
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn no_panic_on_eyj_without_dots() {
        // Partial JWT-shaped prefix with no dots — must not panic or loop.
        let line = "eyJhbGciOiJIUzI1NiJ9xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
        let out = redact(line);
        // No redaction should have happened (incomplete JWT shape).
        assert!(out.contains("eyJ"));
    }

    #[test]
    fn no_panic_on_bearer_at_end_of_line() {
        // "bearer " at the very end of the string (zero-length token).
        let line = "Authorization: Bearer ";
        let _ = redact(line);
    }

    #[test]
    fn no_panic_with_empty_live_token() {
        let out = redact_with_token("some text", Some(""));
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn no_panic_on_repeated_eyj_markers() {
        // Many back-to-back `eyJ` prefixes without valid JWT structure.
        let line = "eyJ".repeat(10_000);
        let _ = redact(&line);
    }

    // ── Cow semantics: borrowed on no-match, owned on match ─────────────────

    #[test]
    fn owned_cow_on_jwt_match() {
        let jwt =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"; // gitleaks:allow
        let out = redact(jwt);
        assert!(matches!(out, Cow::Owned(_)));
    }

    #[test]
    fn borrowed_cow_on_clean_input() {
        let out = redact("no secrets here, just plain text");
        assert!(matches!(out, Cow::Borrowed(_)));
    }
}
