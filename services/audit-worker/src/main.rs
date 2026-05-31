use std::sync::Arc;

use anyhow::{Context as _, Result};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinHandle;

mod audit_consumer;
mod mirror_consumer;

fn validate_boot_env() -> anyhow::Result<()> {
    let mut errors: Vec<String> = Vec::new();

    let db_url = std::env::var("RB_DATABASE_URL").unwrap_or_default();
    if db_url.is_empty() {
        errors.push("RB_DATABASE_URL: required but missing".to_owned());
    } else if !db_url.starts_with("postgres") {
        errors.push(format!(
            "RB_DATABASE_URL: expected postgres DSN, got {db_url:?}"
        ));
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "audit-worker boot validation failed ({} error(s)):\n{}",
            errors.len(),
            errors
                .iter()
                .map(|e| format!("  - {e}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("audit-worker")?;
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => "audit_worker",
        "git_sha" => rb_build_info::SHA,
        "version" => env!("CARGO_PKG_VERSION"),
    )
    .set(1.0);
    let metrics_port: u16 = std::env::var("RB_METRICS_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(9091);
    tokio::spawn(async move {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/metrics",
            get(move || async move { metrics_handle.render() }),
        );
        let listener = tokio::net::TcpListener::bind(("0.0.0.0", metrics_port))
            .await
            .expect("metrics listener bind failed");
        tracing::info!(port = metrics_port, "metrics server listening");
        axum::serve(listener, app)
            .await
            .expect("metrics server error");
    });

    let database_url = std::env::var("RB_DATABASE_URL").context("RB_DATABASE_URL is required")?;

    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .context("failed to connect to Postgres")?,
    );

    tracing::info!("audit-worker starting");

    let audit_handle = spawn_with_log("audit_consumer", || audit_consumer::spawn(&pool))?;

    let mirror_handle = spawn_with_log("mirror_consumer", || mirror_consumer::spawn(&pool))?;

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping consumers");

    audit_handle.abort();
    mirror_handle.abort();

    Ok(())
}

fn spawn_with_log(
    name: &'static str,
    f: impl FnOnce() -> Result<JoinHandle<()>>,
) -> Result<JoinHandle<()>> {
    match f() {
        Ok(h) => {
            tracing::info!("{name} started");
            Ok(h)
        }
        Err(e) => {
            tracing::warn!("{name} failed to start (Kafka unavailable?): {e}");
            Err(e)
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install CTRL+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
