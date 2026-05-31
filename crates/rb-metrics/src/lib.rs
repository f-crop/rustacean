//! Shared Prometheus metrics setup for Rustacean services.
//!
//! Call [`install_recorder`] once at startup, then pass the handle to
//! [`spawn_metrics_server`] (workers) or embed it in an existing Axum router
//! (control-api).
use anyhow::Context;
use metrics_exporter_prometheus::PrometheusHandle;

/// Installs the global Prometheus recorder and emits the `rb_build_info` gauge.
///
/// Must be called once before any metrics are recorded, typically right after
/// tracing is initialised.
///
/// # Errors
///
/// Returns an error if a recorder is already installed globally.
pub fn install_recorder(service_name: &'static str) -> anyhow::Result<PrometheusHandle> {
    let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => service_name,
        "git_sha" => rb_build_info::SHA,
        "version" => env!("CARGO_PKG_VERSION"),
    )
    .set(1.0);
    Ok(handle)
}

/// Spawns a dedicated Axum server exposing `GET /metrics` on `RB_METRICS_PORT`
/// (default 9091).
///
/// Must be called from within a Tokio runtime. The server runs as a background
/// task; bind failures cause a panic in that task.
///
/// # Panics
///
/// The spawned task panics if the TCP listener cannot bind or if Axum's serve
/// loop returns an error. Neither condition is expected in normal operation.
pub fn spawn_metrics_server(handle: PrometheusHandle) {
    let port: u16 = std::env::var("RB_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9091);
    tokio::spawn(async move {
        use axum::routing::get;
        let app =
            axum::Router::new().route("/metrics", get(move || async move { handle.render() }));
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
            .await
            .expect("metrics listener bind failed");
        tracing::info!(port, "metrics server listening");
        axum::serve(listener, app)
            .await
            .expect("metrics server error");
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_info_gauge_emits_nonzero_through_exporter() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, || {
            metrics::gauge!(
                "rb_build_info",
                "service" => "test_svc",
                "git_sha" => rb_build_info::SHA,
                "version" => env!("CARGO_PKG_VERSION"),
            )
            .set(1.0);
        });
        let output = handle.render();
        assert!(
            output.contains("rb_build_info"),
            "rb_build_info gauge missing from rendered output:\n{output}"
        );
        assert!(
            output.contains('1'),
            "gauge value 1 missing from rendered output:\n{output}"
        );
    }
}
