mod error;
mod kafka;
mod pg;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "migrate", about = "rust-brain v2 migration runner", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Path to repository root (default: current directory)
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Postgres connection URL (falls back to `DATABASE_URL` env var; not used for `kafka`)
    #[arg(long)]
    database_url: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Apply control-plane migrations to the `control` schema
    Control,

    /// Apply per-tenant blueprint migrations
    Tenant(TenantArgs),

    /// Create/update Kafka topics from infra/kafka/topics.yaml
    Kafka(KafkaArgs),

    /// Print applied/pending migration status for all schemas
    Status,
}

#[derive(Args, Debug)]
struct TenantArgs {
    /// Tenant ID to migrate (24-char hex string)
    id: Option<String>,

    /// Apply migrations to all existing tenant schemas
    #[arg(long, conflicts_with = "id")]
    all: bool,
}

#[derive(Args, Debug)]
struct KafkaArgs {
    /// Kafka bootstrap servers (falls back to `KAFKA_BOOTSTRAP_SERVERS` env var)
    #[arg(long)]
    bootstrap_servers: Option<String>,

    /// Path to topics YAML file (default: <root>/infra/kafka/topics.yaml)
    #[arg(long)]
    config: Option<PathBuf>,

    /// Print topic status instead of applying changes
    #[arg(long)]
    status: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let root = cli.root.canonicalize().context("resolving --root")?;

    match cli.command {
        Command::Kafka(args) => {
            let brokers = args
                .bootstrap_servers
                .or_else(|| std::env::var("KAFKA_BOOTSTRAP_SERVERS").ok())
                .unwrap_or_else(|| "localhost:9092".to_owned());

            let config_path = args
                .config
                .unwrap_or_else(|| root.join("infra").join("kafka").join("topics.yaml"));

            if args.status {
                kafka::print_status(&brokers, &config_path)?;
            } else {
                let result = kafka::apply_topics(&brokers, &config_path).await?;
                if result.created == 0 && result.configs_applied == 0 {
                    println!(
                        "kafka: all topics already up to date ({} skipped)",
                        result.skipped
                    );
                } else {
                    println!(
                        "kafka: created={} configs_applied={} skipped={}",
                        result.created, result.configs_applied, result.skipped
                    );
                }
            }
        }

        pg_command => {
            let database_url = cli
                .database_url
                .or_else(|| std::env::var("DATABASE_URL").ok())
                .context("DATABASE_URL must be set (via --database-url or DATABASE_URL env var)")?;

            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(5)
                .connect(&database_url)
                .await
                .context("connecting to Postgres")?;

            match pg_command {
                Command::Control => {
                    let count = pg::migrate_control(&pool, &root).await?;
                    if count == 0 {
                        println!("control: already up to date");
                    } else {
                        println!("control: applied {count} migration(s)");
                    }
                }

                Command::Tenant(args) => match (args.id, args.all) {
                    (Some(id), _) => {
                        let count = pg::migrate_tenant(&pool, &id, &root).await?;
                        if count == 0 {
                            println!("tenant_{id}: already up to date");
                        } else {
                            println!("tenant_{id}: applied {count} migration(s)");
                        }
                    }
                    (None, true) => {
                        let count = pg::migrate_all_tenants(&pool, &root).await?;
                        println!("all tenants: applied {count} migration(s) total");
                    }
                    (None, false) => {
                        anyhow::bail!("migrate tenant: provide a tenant ID or --all");
                    }
                },

                Command::Status => {
                    let control = pg::control_status(&pool, &root).await?;
                    println!("=== control ===");
                    for s in &control {
                        let state = if s.applied { "applied" } else { "pending" };
                        println!("  v{:03} {:30} {}", s.version, s.description, state);
                    }

                    let schemas = pg::tenant_schemas(&pool).await?;
                    for schema in &schemas {
                        let tenant_id = schema.trim_start_matches("tenant_");
                        let rows = pg::tenant_status(&pool, tenant_id, &root).await?;
                        println!("=== {schema} ===");
                        for s in &rows {
                            let state = if s.applied { "applied" } else { "pending" };
                            println!("  v{:03} {:30} {}", s.version, s.description, state);
                        }
                    }
                }

                Command::Kafka(_) => unreachable!("handled above"),
            }
        }
    }

    Ok(())
}
