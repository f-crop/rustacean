//! Synthetic-load harness for Wave 8 7-day pre-prod exit run.
//!
//! ADR-012 §2.7 — Platform Engineer implementation.
//!
//! Usage:
//!   synthetic-load start        # run the harness
//!   synthetic-load provision    # only provision tenant pool, then exit
//!   synthetic-load report       # print the latest daily summary and exit
//!   synthetic-load build-info   # print compile-time build provenance and exit

use std::sync::Arc;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

mod client;
mod config;
mod health;
mod loops;
mod report;
mod state;
mod tenant;

use config::Config;
use state::HarnessState;

#[derive(Parser)]
#[command(name = "synthetic-load", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the harness and run for the configured soak duration.
    Start,

    /// Provision the tenant pool only, then exit. Useful for pre-flight.
    Provision,

    /// Print the most recent daily summary from the state directory and exit.
    Report,

    /// Print compile-time build provenance as JSON and exit.
    BuildInfo,
}

fn validate_boot_env(config: &Config) -> Result<()> {
    let mut errors: Vec<String> = Vec::new();

    if config.target_url.is_empty() {
        errors.push("SYNTHETIC_LOAD_TARGET: required but empty".to_owned());
    }
    if config.admin_token.is_empty() {
        errors.push("SYNTHETIC_LOAD_ADMIN_TOKEN: required but empty".to_owned());
    }
    if config.database_url.is_empty() {
        errors.push("SYNTHETIC_LOAD_DATABASE_URL or DATABASE_URL: required but empty".to_owned());
    }

    if !errors.is_empty() {
        anyhow::bail!(
            "synthetic-load boot validation failed ({} error(s)):\n{}",
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
    let cli = Cli::parse();

    if let Command::BuildInfo = cli.command {
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

    let config = Config::from_env().context("failed to load config from environment")?;

    match cli.command {
        Command::Report => {
            return print_latest_report(&config);
        }
        Command::BuildInfo => unreachable!(),
        _ => {}
    }

    validate_boot_env(&config)?;

    let _guard = rb_tracing::init("synthetic-load")?;
    rb_metrics::spawn_metrics_server(rb_metrics::install_recorder("synthetic-load")?);

    tracing::info!(
        target = %config.target_url,
        tenant_count = config.tenant_count,
        soak_days = config.soak_duration.as_secs() / 86400,
        "synthetic-load harness starting"
    );

    let state = Arc::new(Mutex::new(
        HarnessState::load_or_new(&config.state_dir).context("failed to load harness state")?,
    ));
    let config = Arc::new(config);
    let cancel = CancellationToken::new();

    match cli.command {
        Command::Provision => {
            provision_only(&config, &state).await?;
            tracing::info!("provision complete");
        }
        Command::Start => {
            let cancel_clone = cancel.clone();
            let cancel_signal = cancel.clone();

            // Wire graceful shutdown.
            tokio::spawn(async move {
                shutdown_signal().await;
                tracing::info!("shutdown signal received");
                cancel_signal.cancel();
            });

            // Enforce soak duration.
            let soak_dur = config.soak_duration;
            let cancel_timer = cancel.clone();
            tokio::spawn(async move {
                tokio::time::sleep(soak_dur).await;
                tracing::info!("soak duration elapsed; initiating clean shutdown");
                cancel_timer.cancel();
            });

            let result = loops::run(Arc::clone(&config), Arc::clone(&state), cancel_clone).await;

            // Save final state.
            {
                let mut s = state.lock().await;
                if let Err(e) = s.save(&config.state_dir) {
                    tracing::warn!("final state save failed: {e}");
                }
            }

            return result;
        }
        Command::BuildInfo | Command::Report => unreachable!(),
    }

    Ok(())
}

async fn provision_only(config: &Arc<Config>, state: &Arc<Mutex<HarnessState>>) -> Result<()> {
    let admin_http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    loops::ensure_tenant_pool_pub(config, state, &admin_http).await
}

fn print_latest_report(config: &Config) -> Result<()> {
    let dir = &config.state_dir;
    // Find the most recently modified JSON file (excluding harness-state.json).
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read state dir {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .filter(|e| {
            let n = e.file_name();
            let name = n.to_string_lossy();
            name.ends_with(".json")
                && name != "harness-state.json"
                && name != "harness-state.json.tmp"
        })
        .collect();

    entries.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));

    let Some(entry) = entries.first() else {
        anyhow::bail!("no daily summary files found in {}", dir.display());
    };

    let bytes = std::fs::read(entry.path())
        .with_context(|| format!("failed to read {}", entry.path().display()))?;
    println!("{}", String::from_utf8_lossy(&bytes));
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("CTRL+C handler error: {e}");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(e) => {
                tracing::warn!("SIGTERM handler install failed: {e}; SIGTERM will not trigger shutdown");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}
