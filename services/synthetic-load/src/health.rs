use serde::Deserialize;

use crate::client::ApiClient;
use crate::report::{KAFKA_LAG_THRESHOLD, OUTBOX_AGE_THRESHOLD_SECS};

/// Result of one health-check pass.
#[derive(Debug, Default)]
pub struct HealthCheckResult {
    /// Total services polled.
    pub total: u32,
    /// Services that returned 200.
    pub up: u32,
    /// Human-readable detail for any failures.
    pub failures: Vec<String>,
    /// Drift events: SHA changed since the harness started.
    pub drift_events: Vec<String>,
    /// `rb_outbox_age_seconds` p95 if available (scraped from Prometheus).
    pub outbox_age_p95_secs: Option<f64>,
    /// Highest `rb_kafka_consumer_lag` sample seen.
    pub kafka_max_consumer_lag: Option<u64>,
    /// Whether outbox / lag are above degradation thresholds.
    pub degradation_events: Vec<String>,
}

impl HealthCheckResult {
    /// Availability = up / total × 100.
    pub fn availability_pct(&self) -> f64 {
        if self.total == 0 {
            return 100.0;
        }
        (f64::from(self.up) / f64::from(self.total)) * 100.0
    }
}

/// Run one health-check pass against all configured service URLs.
///
/// `expected_sha` is the SHA captured at harness start; drift is flagged when
/// any service returns a different SHA.
pub async fn run_health_check(
    client: &ApiClient,
    service_urls: &[String],
    expected_sha: Option<&str>,
    prometheus_url: Option<&str>,
) -> HealthCheckResult {
    let mut result = HealthCheckResult::default();

    for url in service_urls {
        result.total += 1;
        let (up, detail) = client.health_check(url).await;
        if up {
            result.up += 1;
        } else if let Some(d) = detail {
            result.failures.push(d);
        }

        // Build SHA drift detection
        if let Some(sha) = client.health_build_sha(url).await {
            if let Some(expected) = expected_sha {
                if sha != expected && expected != "unknown" && sha != "unknown" {
                    result
                        .drift_events
                        .push(format!("{url} SHA changed: expected {expected}, got {sha}"));
                }
            }
        }
    }

    // Prometheus metric scrape
    if let Some(prom) = prometheus_url {
        if let Some(age) = query_prometheus_scalar(prom, "rb_outbox_age_seconds").await {
            result.outbox_age_p95_secs = Some(age);
            if age > OUTBOX_AGE_THRESHOLD_SECS {
                result.degradation_events.push(format!(
                    "rb_outbox_age_seconds p95 = {age:.1} s > {OUTBOX_AGE_THRESHOLD_SECS} s threshold"
                ));
            }
        }
        if let Some(lag) = query_prometheus_scalar(prom, "rb_kafka_consumer_lag").await {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let lag_u64 = lag.round() as u64;
            result.kafka_max_consumer_lag = Some(lag_u64);
            if lag_u64 > KAFKA_LAG_THRESHOLD {
                result.degradation_events.push(format!(
                    "rb_kafka_consumer_lag = {lag_u64} > {KAFKA_LAG_THRESHOLD} threshold"
                ));
            }
        }
    }

    result
}

/// Query Prometheus HTTP API for a metric's current value.
///
/// Returns the first scalar value found, or `None` on any error.
async fn query_prometheus_scalar(prom_url: &str, metric: &str) -> Option<f64> {
    #[derive(Deserialize)]
    struct PromResp {
        data: PromData,
    }
    #[derive(Deserialize)]
    struct PromData {
        result: Vec<PromResult>,
    }
    #[derive(Deserialize)]
    struct PromResult {
        value: (f64, String),
    }

    let url = format!(
        "{}/api/v1/query?query={}",
        prom_url.trim_end_matches('/'),
        urlencoding_simple(metric)
    );

    let resp: PromResp = reqwest::get(&url).await.ok()?.json().await.ok()?;
    resp.data
        .result
        .first()
        .and_then(|r| r.value.1.parse::<f64>().ok())
}

fn urlencoding_simple(s: &str) -> String {
    s.replace(' ', "+")
}
