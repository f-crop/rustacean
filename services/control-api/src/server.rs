use std::sync::Arc;

use anyhow::{Context as _, Result};
use axum::routing::get;
use base64::Engine as _;
use jsonwebtoken::EncodingKey;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::{SmtpConfig, from_transport};
use rb_github::{GhApp, Secret};
use rb_kafka::{ConsumerCfg, Producer, ProducerCfg};
use rb_sse::{EventBus, SseConfig};
use rb_storage_neo4j::TenantGraph;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::postgres::PgPoolOptions;
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, SetRequestIdLayer},
    trace::TraceLayer,
};

use rb_schemas::AgentSessionCommand;

use crate::{
    config::Config,
    ingest_consumer,
    middleware,
    routes,
    state::{AgentRegistry, AppState, KafkaConsistencyState, McpSessionStore},
};

/// Connects to Postgres, builds [`AppState`], and drives the server until shutdown.
///
/// # Errors
///
/// Returns an error if the database connection fails, the TCP listener cannot
/// bind, or axum returns an IO error during serving.
#[allow(clippy::too_many_lines)]
pub async fn run(config: Config) -> Result<()> {
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&config.database_url)
        .await
        .context("failed to connect to Postgres")?;

    let smtp_config = SmtpConfig {
        host: std::env::var("RB_SMTP_HOST").unwrap_or_default(),
        port: std::env::var("RB_SMTP_PORT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(587),
        username: std::env::var("RB_SMTP_USER").unwrap_or_default(),
        password: std::env::var("RB_SMTP_PASS").unwrap_or_default(),
        from_address: std::env::var("RB_SMTP_FROM")
            .unwrap_or_else(|_| "noreply@rust-brain.app".to_owned()),
    };
    let email_sender = from_transport(&config.email_transport, &smtp_config)
        .context("failed to build email sender")?;

    let hasher = PasswordHasher::from_config(
        config.argon2_memory_kb,
        config.argon2_time_cost,
        config.argon2_parallelism,
    )
    .context("invalid argon2 parameters")?;

    let gh = build_gh_app(&config)?;

    let gh = gh.map(Arc::new);
    if let Some(g) = &gh {
        // Spawn the installation-token cache sweep now that we are inside
        // the tokio runtime (REQ-GH-05).
        g.start_token_sweep();
    }

    let sse_bus = Arc::new(EventBus::new(SseConfig::default()));

    // Build the Kafka producers.  Failure is non-fatal — routes degrade to 503.
    let producer_cfg = ProducerCfg {
        bootstrap_servers: config.kafka_bootstrap_servers.clone(),
        ..ProducerCfg::default()
    };
    let ingest_producer = build_producer(Producer::new(&producer_cfg), "ingest_producer");
    let tombstone_producer = build_producer(Producer::new(&producer_cfg), "tombstone_producer");

    // Connect to Neo4j.  Failure is non-fatal — graph endpoints degrade to 503.
    let graph = if let (Some(uri), Some(password)) =
        (config.neo4j_uri.as_deref(), config.neo4j_password.as_deref())
    {
        match TenantGraph::connect(uri, &config.neo4j_user, password).await {
            Ok(g) => {
                tracing::info!("neo4j connected at {uri}");
                Some(Arc::new(g))
            }
            Err(e) => {
                tracing::warn!("neo4j connection failed (graph endpoints disabled): {e}");
                None
            }
        }
    } else {
        tracing::info!("RB_NEO4J_URI / RB_NEO4J_PASSWORD not set — graph endpoints disabled");
        None
    };

    // Build the Qdrant vector store.  Failure is non-fatal — `/v1/search`
    // returns 503 when Qdrant is not configured.
    let qdrant = config.qdrant_url.as_deref().map(|url| {
        tracing::info!("qdrant configured at {url}");
        Arc::new(TenantVectorStore::new(url))
    });

    let kafka_consistency = Arc::new(KafkaConsistencyState::new());

    // Build agent commands Kafka producer. Failure is non-fatal — agent routes
    // return 503 when Kafka is not configured.
    let agent_commands_producer = build_producer(
        Producer::<AgentSessionCommand>::new(&producer_cfg),
        "agent_commands_producer",
    );

    let state = AppState {
        pool,
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config.clone()),
        gh,
        sse_bus: Arc::clone(&sse_bus),
        ingest_producer,
        tombstone_producer,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph,
        qdrant,
        http_client: reqwest::Client::new(),
        neo4j_uri: config.neo4j_uri.clone(),
        kafka_consistency: Arc::clone(&kafka_consistency),
        mcp_sessions: McpSessionStore,
        agent_registry: AgentRegistry::new(),
        agent_commands_producer,
    };

    // Spawn the Kafka → SSE fan-out consumer.  Errors here are logged but do
    // not prevent the HTTP server from starting — the SSE endpoint degrades
    // gracefully when Kafka is unavailable (no events; long-poll returns empty).
    let consumer_cfg = ConsumerCfg::new("control-api-sse");
    match ingest_consumer::spawn(&consumer_cfg, sse_bus, Arc::new(state.pool.clone()), kafka_consistency) {
        Ok(_handle) => tracing::info!("ingest_consumer started"),
        Err(e) => tracing::warn!("ingest_consumer failed to start (Kafka unavailable?): {e}"),
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::build(state)
        .route("/metrics", get(move || async move { metrics_handle.render() }))
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(cors)
        .layer(axum::middleware::from_fn(
            middleware::otel_trace::otel_trace_middleware,
        ));

    let addr: std::net::SocketAddr = config.listen_addr.parse()?;
    tracing::info!(addr = %addr, "control-api listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn build_producer<P, Err: std::fmt::Display>(
    result: Result<P, Err>,
    name: &str,
) -> Option<Arc<P>> {
    match result {
        Ok(p) => {
            tracing::info!("{name} connected to Kafka");
            Some(Arc::new(p))
        }
        Err(e) => {
            tracing::warn!("{name} failed to connect (Kafka unavailable?): {e}");
            None
        }
    }
}

/// Constructs a [`GhApp`] from config, or returns `None` when the GitHub App
/// env vars are not set (feature is disabled; GitHub routes return 503).
///
/// Fails fast at startup if keys are present but malformed — an operator
/// mistake should surface immediately, not at first API call.
fn build_gh_app(config: &Config) -> Result<Option<GhApp>> {
    let (Some(app_id), Some(pem_b64)) =
        (config.gh_app_id, config.gh_app_private_key_b64.as_deref())
    else {
        tracing::info!("RB_GH_APP_ID / RB_GH_APP_PRIVATE_KEY not set — GitHub App disabled");
        return Ok(None);
    };

    let pem = base64::engine::general_purpose::STANDARD
        .decode(pem_b64)
        .context("RB_GH_APP_PRIVATE_KEY must be base64-encoded PEM")?;

    let encoding_key = EncodingKey::from_rsa_pem(&pem)
        .context("RB_GH_APP_PRIVATE_KEY must be a valid RSA PEM private key")?;

    // Zeroize raw PEM bytes now that the opaque key has been derived.
    drop(pem);

    let webhook_secret_bytes = config
        .gh_app_webhook_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "RB_GH_APP_WEBHOOK_SECRET must be set when GitHub App is enabled. \
                 An absent or empty webhook secret allows any caller to forge webhook \
                 payloads — set this env var before enabling real webhook delivery."
            )
        })?
        .as_bytes()
        .to_vec();
    let webhook_secret = Secret::new(webhook_secret_bytes);

    Ok(Some(GhApp::new(app_id, encoding_key, webhook_secret)))
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
