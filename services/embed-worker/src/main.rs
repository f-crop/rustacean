use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use rb_blob::store_from_env;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{IngestStatusEvent, TypecheckedItemEvent};

mod consumer;
mod embedder;
mod qdrant;

#[derive(Parser)]
#[command(name = "embed-worker", version)]
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

    let blob_store = std::env::var("RB_BLOB_STORE").unwrap_or_else(|_| "filesystem".to_owned());
    if !matches!(blob_store.as_str(), "filesystem" | "s3") {
        errors.push(format!(
            "RB_BLOB_STORE={blob_store:?}: must be 'filesystem' or 's3'"
        ));
    }

    let dims = std::env::var("RB_EMBEDDING_DIMENSIONS").unwrap_or_else(|_| "768".to_owned());
    if dims.parse::<u32>().is_err() || dims == "0" {
        errors.push(format!(
            "RB_EMBEDDING_DIMENSIONS={dims:?}: must be a positive integer"
        ));
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "embed-worker boot validation failed ({} error(s)):\n{}",
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

    let _guard = rb_tracing::init("embed-worker")?;
    rb_metrics::spawn_metrics_server(rb_metrics::install_recorder("embed_worker")?);

    let ollama_url =
        std::env::var("RB_OLLAMA_URL").unwrap_or_else(|_| "http://ollama:11434".to_owned());
    let embedding_model =
        std::env::var("RB_EMBEDDING_MODEL").unwrap_or_else(|_| "nomic-embed-text".to_owned());
    let embedding_dimensions: u32 = std::env::var("RB_EMBEDDING_DIMENSIONS")
        .unwrap_or_else(|_| "768".to_owned())
        .parse()
        .context("RB_EMBEDDING_DIMENSIONS must be a positive integer")?;
    let qdrant_url =
        std::env::var("QDRANT_URL").unwrap_or_else(|_| "http://qdrant:6333".to_owned());

    // Fail fast: validate that Qdrant collection dimensions match our config.
    qdrant::ensure_collection(&qdrant_url, embedding_dimensions)
        .await
        .context("Qdrant startup check failed")?;

    tracing::info!(
        embedding_model,
        embedding_dimensions,
        "embed-worker: Qdrant collection validated"
    );

    let blob_store = store_from_env()
        .await
        .context("failed to init blob store")?;

    let embed_cfg = ConsumerCfg::new("embed-worker");
    let item_consumer: Consumer<TypecheckedItemEvent> = Consumer::new(&embed_cfg)?;
    item_consumer.subscribe(&[consumer::TOPIC_EMBED_COMMANDS])?;
    let item_consumer = KafkaHealthWatcher::wrap(
        item_consumer,
        &embed_cfg,
        &[consumer::TOPIC_EMBED_COMMANDS.to_owned()],
        WatchdogConfig::default(),
    );

    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("embed-worker starting");

    let handle = tokio::spawn(consumer::run(
        item_consumer,
        blob_store,
        status_producer,
        ollama_url,
        embedding_model,
        embedding_dimensions,
        qdrant_url,
    ));

    shutdown_signal().await;
    tracing::info!("shutdown signal received — stopping consumer");
    handle.abort();

    Ok(())
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
