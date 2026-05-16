use std::sync::Arc;

use anyhow::{Context as _, Result};
use rb_blob::store_from_env;
use rb_kafka::{Consumer, ConsumerCfg, Producer, ProducerCfg};
use rb_kafka_health::{KafkaHealthWatcher, WatchdogConfig};
use rb_schemas::{IngestRequest, IngestStatusEvent, TypecheckedItemEvent};

mod consumer;
mod helpers;
mod type_extractor;

fn validate_boot_env() -> Result<()> {
    let blob_store = std::env::var("RB_BLOB_STORE").unwrap_or_else(|_| "filesystem".to_owned());
    if !matches!(blob_store.as_str(), "filesystem" | "s3") {
        anyhow::bail!(
            "typecheck-worker boot validation failed:\n  \
             - RB_BLOB_STORE={blob_store:?}: must be 'filesystem' or 's3'"
        );
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    validate_boot_env()?;

    let _guard = rb_tracing::init("typecheck-worker")?;

    let blob_store = store_from_env()
        .await
        .context("failed to init blob store")?;

    let cfg = ConsumerCfg::new("typecheck-worker");
    let cmd_consumer: Consumer<IngestRequest> = Consumer::new(&cfg)?;
    cmd_consumer.subscribe(&[consumer::TOPIC_TYPECHECK_COMMANDS])?;
    let cmd_consumer = KafkaHealthWatcher::wrap(
        cmd_consumer,
        &cfg,
        &[consumer::TOPIC_TYPECHECK_COMMANDS.to_owned()],
        WatchdogConfig::default(),
    );

    let item_producer = Arc::new(Producer::<TypecheckedItemEvent>::new(
        &ProducerCfg::default(),
    )?);
    let status_producer = Arc::new(Producer::<IngestStatusEvent>::new(&ProducerCfg::default())?);

    tracing::info!("typecheck-worker starting");

    let handle = tokio::spawn(consumer::run(
        cmd_consumer,
        blob_store,
        item_producer,
        status_producer,
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
