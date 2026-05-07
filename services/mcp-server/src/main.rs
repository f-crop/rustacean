use anyhow::Result;
use clap::Parser;
use mcp_server::Config;

#[derive(Parser)]
#[command(name = "mcp-server", about = "Rustbrain MCP Server (REQ-MC-01)", version)]
struct Cli {
    /// Start the MCP HTTP server.
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Start the HTTP server (default).
    Serve,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Command::Serve) {
        Command::Serve => {
            let config = Config::from_env()?;
            config.validate()?;
            let _guard = rb_tracing::init(&config.service_name)?;
            mcp_server::run(config).await
        }
    }
}
