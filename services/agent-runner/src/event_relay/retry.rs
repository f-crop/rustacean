//! Exponential-backoff HTTP POST for the event relay.
//!
//! Extracted from `event_relay` to keep that module under the 600-line cap.
//! Contains [`post_with_retry`] and its unit tests.

use std::time::Duration;

use metrics::counter;

// ---------------------------------------------------------------------------
// Retry constants
// ---------------------------------------------------------------------------

pub(crate) const MAX_RETRY_ATTEMPTS: u32 = 5;

#[cfg(not(test))]
pub(crate) const BASE_RETRY_DELAY_MS: u64 = 100;
#[cfg(test)]
pub(crate) const BASE_RETRY_DELAY_MS: u64 = 1; // fast retries in unit tests

// ---------------------------------------------------------------------------
// HTTP POST with exponential backoff retry
// ---------------------------------------------------------------------------

pub(crate) async fn post_with_retry(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
    session_id: &str,
    event_count: usize,
) {
    for attempt in 0..MAX_RETRY_ATTEMPTS {
        if attempt > 0 {
            let delay_ms = BASE_RETRY_DELAY_MS * (1u64 << (attempt - 1));
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            counter!("rb_agent_relay_retry_total").increment(1);
        }

        match client.post(url).json(body).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    session_id = %session_id,
                    event_count = event_count,
                    "EventRelay: batch flushed"
                );
                counter!("rb_agent_relay_published_total").increment(event_count as u64);
                return;
            }
            Ok(resp) if resp.status().is_server_error() => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempt + 1,
                    status = %resp.status(),
                    event_count = event_count,
                    "EventRelay: POST 5xx — will retry"
                );
            }
            Ok(resp) => {
                // 4xx and other non-retryable responses
                tracing::warn!(
                    session_id = %session_id,
                    status = %resp.status(),
                    event_count = event_count,
                    "EventRelay: POST failed (non-retryable) — dropping batch"
                );
                counter!("rb_agent_relay_failed_total").increment(event_count as u64);
                return;
            }
            Err(e) if e.is_connect() || e.is_timeout() => {
                tracing::warn!(
                    session_id = %session_id,
                    attempt = attempt + 1,
                    error = %e,
                    event_count = event_count,
                    "EventRelay: connection error — will retry"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    event_count = event_count,
                    "EventRelay: POST error (non-retryable) — dropping batch"
                );
                counter!("rb_agent_relay_failed_total").increment(event_count as u64);
                return;
            }
        }
    }

    tracing::error!(
        session_id = %session_id,
        event_count = event_count,
        max_attempts = MAX_RETRY_ATTEMPTS,
        "EventRelay: batch dropped after exhausting all retry attempts"
    );
    counter!("rb_agent_relay_failed_total").increment(event_count as u64);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retries_once_on_500_then_succeeds_on_2xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;

        // First request → 500; subsequent → 204
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s1/events"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s1/events"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s1/events", server.uri());

        post_with_retry(&client, &url, &body, "s1", 0).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(
            n, 2,
            "should have made exactly 2 requests (1 failure + 1 success)"
        );
    }

    #[tokio::test]
    async fn exhausts_all_retry_attempts_on_persistent_5xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s2/events"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s2/events", server.uri());

        post_with_retry(&client, &url, &body, "s2", 5).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(
            n, MAX_RETRY_ATTEMPTS as usize,
            "must exhaust all {MAX_RETRY_ATTEMPTS} retry attempts"
        );
    }

    #[tokio::test]
    async fn does_not_retry_on_4xx_client_error() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s3/events"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s3/events", server.uri());

        post_with_retry(&client, &url, &body, "s3", 1).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(n, 1, "4xx must not trigger any retries");
    }

    #[tokio::test]
    async fn succeeds_on_first_attempt_makes_exactly_one_request() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s4/events"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s4/events", server.uri());

        post_with_retry(&client, &url, &body, "s4", 3).await;

        let n = server.received_requests().await.unwrap().len();
        assert_eq!(n, 1, "success on first attempt — no retries");
    }

    // ── Metric correctness ─────────────────────────────────────────────────

    /// Regression: attempt-0 failures must NOT increment `retry_total`.
    /// A single 5xx on attempt=0 followed by a 2xx success on attempt=1
    /// must produce `retry_total` == 1 (not 2).
    #[tokio::test]
    async fn retry_total_not_incremented_on_attempt_zero() {
        use metrics_util::debugging::{DebugValue, DebuggingRecorder};
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        // A fresh recorder per test; ignore errors if another test already
        // installed a global recorder (the assertion below is what matters).
        let _ = recorder.install();

        let server = MockServer::start().await;
        // attempt 0 → 500, attempt 1 → 200
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s5/events"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/internal/agent/sessions/s5/events"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let body = serde_json::json!({ "tenant_id": "t1", "events": [] });
        let url = format!("{}/internal/agent/sessions/s5/events", server.uri());

        post_with_retry(&client, &url, &body, "s5", 1).await;

        let entries = snapshotter.snapshot().into_vec();
        let retry_count: u64 = entries
            .iter()
            .filter(|(key, _, _, _)| key.key().name() == "rb_agent_relay_retry_total")
            .filter_map(|(_, _, _, v)| {
                if let DebugValue::Counter(n) = v {
                    Some(*n)
                } else {
                    None
                }
            })
            .sum();

        // Only attempt=1 (the real retry) must be counted.
        assert_eq!(
            retry_count, 1,
            "retry_total must be 1 (attempt=1 only); attempt=0 failure must not count"
        );
    }
}
