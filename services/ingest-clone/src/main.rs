use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use jsonwebtoken::EncodingKey;
use rb_blob::store_from_env;
use rb_github::GhApp;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{IngestRequest, IngestStatusEvent, SourceFileEvent};
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinHandle;

mod consumer;

#[derive(Parser)]
#[command(name = "ingest-clone", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print compile-time build provenance as JSON and exit.
    BuildInfo,
}

fn validate_boot_env() -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    let db_url = std::env::var("DATABASE_URL").unwrap_or_default();
    if db_url.is_empty() {
        errors.push("DATABASE_URL: required but missing".to_owned());
    } else if !db_url.starts_with("postgres") {
        errors.push(format!(
            "DATABASE_URL: expected postgres DSN, got {db_url:?}"
        ));
    }

    // When GITHUB_APP_ID is set, GITHUB_APP_PRIVATE_KEY_PEM is required and must
    // be a raw PEM block (not base64) — ingest-clone passes it directly to jsonwebtoken.
    let app_id = std::env::var("GITHUB_APP_ID").unwrap_or_default();
    if !app_id.is_empty() {
        let pem = std::env::var("GITHUB_APP_PRIVATE_KEY_PEM").unwrap_or_default();
        if pem.is_empty() {
            errors
                .push("GITHUB_APP_PRIVATE_KEY_PEM: required when GITHUB_APP_ID is set".to_owned());
        } else if !pem.contains("BEGIN") || !pem.contains("PRIVATE") {
            errors.push(
                "GITHUB_APP_PRIVATE_KEY_PEM: missing PEM header ('BEGIN ... PRIVATE KEY'). \
                 This var takes raw PEM, not base64."
                    .to_owned(),
            );
        }
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "ingest-clone boot validation failed ({} error(s)):\n{}",
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
    if let Some(Command::BuildInfo) = Cli::parse().command {
        let info = rb_build_info::get();
        println!(
            "{}",
            serde_json::json!({
                "sha": info.sha,
                "timestamp": info.timestamp,
                "dirty": info.dirty,
            })
        );
        return Ok(());
    }

    validate_boot_env()?;

    let _guard = rb_tracing::init("ingest-clone")?;
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;
    metrics::gauge!(
        "rb_build_info",
        "service" => "ingest_clone",
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

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(5)
            .connect(&database_url)
            .await
            .context("failed to connect to Postgres")?,
    );

    let gh_app = build_gh_app()?.map(Arc::new);

    let blob_store = store_from_env()
        .await
        .context("failed to init blob store")?;

    let consumer_cfg = ConsumerCfg::new("ingest-clone-worker");
    let consumer: Consumer<IngestRequest> = Consumer::new(&consumer_cfg)?;
    consumer.subscribe(&[consumer::TOPIC_CLONE_COMMANDS])?;
    let consumer = KafkaHealthWatcher::wrap(
        consumer,
        &consumer_cfg,
        &[consumer::TOPIC_CLONE_COMMANDS.to_owned()],
        WatchdogConfig::default(),
    );

    let source_producer = Arc::new(Producer::<SourceFileEvent>::new(&ProducerCfg::default())?);
    let expand_producer = Arc::new(Producer::<IngestRequest>::new(&ProducerCfg::default())?);
    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("ingest-clone starting");

    let handle: JoinHandle<()> = tokio::spawn(consumer::run(
        consumer,
        pool,
        gh_app, // Option<Arc<GhApp>>
        blob_store,
        source_producer,
        expand_producer,
        status_producer,
    ));

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping consumer");
    handle.abort();

    Ok(())
}

fn build_gh_app() -> Result<Option<GhApp>> {
    let app_id_str = std::env::var("GITHUB_APP_ID").unwrap_or_default();
    if app_id_str.is_empty() {
        tracing::info!(
            "GITHUB_APP_ID is unset — GitHub App auth disabled; using PAT or public clone"
        );
        return Ok(None);
    }

    let app_id: i64 = app_id_str
        .parse()
        .context("GITHUB_APP_ID must be a number")?;

    let private_key_pem = std::env::var("GITHUB_APP_PRIVATE_KEY_PEM")
        .context("GITHUB_APP_PRIVATE_KEY_PEM is required when GITHUB_APP_ID is set")?;

    let encoding_key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .context("invalid GITHUB_APP_PRIVATE_KEY_PEM")?;

    let webhook_secret_raw = std::env::var("GITHUB_WEBHOOK_SECRET")
        .unwrap_or_default()
        .into_bytes();

    Ok(Some(GhApp::new(
        app_id,
        encoding_key,
        rb_github::Secret::new(webhook_secret_raw),
    )))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_gh_app_returns_none_when_app_id_absent() {
        // SAFETY: single-threaded test binary; no other thread reads GITHUB_APP_ID.
        unsafe { std::env::remove_var("GITHUB_APP_ID") };
        let result =
            build_gh_app().expect("build_gh_app must not error when GITHUB_APP_ID is absent");
        assert!(result.is_none());
    }

    #[test]
    fn build_gh_app_returns_none_when_app_id_empty_string() {
        // SAFETY: single-threaded test binary; no other thread reads GITHUB_APP_ID.
        unsafe { std::env::set_var("GITHUB_APP_ID", "") };
        let result =
            build_gh_app().expect("build_gh_app must not error when GITHUB_APP_ID is empty");
        assert!(result.is_none());
    }
}
