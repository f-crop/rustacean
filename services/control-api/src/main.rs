// dev-watch probe 2026-06-17
use anyhow::Result;
use clap::{Parser, Subcommand};
use control_api::Config;
use utoipa::OpenApi as _;

#[derive(Parser)]
#[command(
    name = "control-api",
    about = "rust-brain control-plane HTTP API",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Start the HTTP server (default when no subcommand given).
    Serve,
    /// Print the `OpenAPI` spec as JSON and exit (used by CI openapi-sync job).
    PrintOpenapi,
    /// Print compile-time build provenance (SHA, timestamp, dirty) as JSON and exit.
    BuildInfo,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Serve) {
        Command::PrintOpenapi => {
            let spec = control_api::ApiDoc::openapi();
            println!("{}", serde_json::to_string_pretty(&spec)?);
            Ok(())
        }
        Command::BuildInfo => {
            let info = rb_build_info::get();
            println!(
                "{}",
                serde_json::json!({
                    "sha": info.sha,
                    "timestamp": info.timestamp,
                    "dirty": info.dirty,
                })
            );
            Ok(())
        }
        Command::Serve => {
            let config = Config::from_env()?;
            config.validate()?;
            let _guard = rb_tracing::init(&config.service_name)?;
            control_api::run(config).await
        }
    }
}
