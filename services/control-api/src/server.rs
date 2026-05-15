use std::sync::Arc;

use anyhow::{Context as _, Result};
use axum::routing::get;
use base64::Engine as _;
use jsonwebtoken::EncodingKey;
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::{SmtpConfig, from_transport};
use rb_github::{AppConfigStore, EncryptionKey, GhApp, GhAppLoader, Secret, try_build_gh_app};
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
    ingest_consumer, jobs, middleware, routes,
    state::{
        AgentRegistry, AppState, KafkaConsistencyState, McpSessionStore, SessionCreateRateLimiter,
        TenantSessionCount,
    },
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

    // GitHub App resolution order (Phase 2 of the Manifest flow):
    //   1. If RB_GH_APP_ENC_KEY is set AND control.github_app_config has an
    //      active row, build the App from the DB row (decrypt-on-read).
    //   2. Otherwise, fall back to the env-var path (RB_GH_APP_ID +
    //      RB_GH_APP_PRIVATE_KEY + RB_GH_APP_WEBHOOK_SECRET).
    //   3. Otherwise, the loader holds None and GitHub routes return 503
    //      via the existing GithubAppNotConfigured path.
    //
    // Phase 3's admin callback will hot-swap the loader without restart by
    // calling GhAppLoader::set after a successful manifest exchange.
    let gh = resolve_gh_app(&config, &pool).await?;
    let gh = gh.map(Arc::new);
    if let Some(g) = &gh {
        // Spawn the installation-token cache sweep now that we are inside
        // the tokio runtime (REQ-GH-05).
        g.start_token_sweep();
    }
    let gh_loader = Arc::new(GhAppLoader::new(gh));

    let sse_bus = Arc::new(EventBus::new(SseConfig::default()));

    // Build the Kafka producers.  Failure is non-fatal — routes degrade to 503.
    let producer_cfg = ProducerCfg {
        bootstrap_servers: config.kafka_bootstrap_servers.clone(),
        ..ProducerCfg::default()
    };
    let ingest_producer = build_producer(Producer::new(&producer_cfg), "ingest_producer");
    let tombstone_producer = build_producer(Producer::new(&producer_cfg), "tombstone_producer");

    // Connect to Neo4j.  Failure is non-fatal — graph endpoints degrade to 503.
    let graph = if let (Some(uri), Some(password)) = (
        config.neo4j_uri.as_deref(),
        config.neo4j_password.as_deref(),
    ) {
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
        gh_loader,
        sse_bus: Arc::clone(&sse_bus),
        ingest_producer,
        tombstone_producer,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph,
        qdrant,
        http_client: reqwest::Client::new(),
        neo4j_uri: config.neo4j_uri.clone(),
        kafka_consistency: Arc::clone(&kafka_consistency),
        mcp_sessions: McpSessionStore::new(),
        agent_registry: AgentRegistry::new(),
        agent_commands_producer,
        internal_secret: config.internal_secret.clone().unwrap_or_default(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::new(
            config.session_create_rate_limit,
            config.session_create_window_secs,
        )),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
    };

    // Spawn the periodic reconciler that heals ingestion runs left in `queued`
    // by a crash or Kafka publish failure.  Runs every 2 minutes throughout
    // the lifetime of the process (not just on startup).
    drop(jobs::spawn_reconciler_loop(state.clone()));

    // Spawn the Kafka → SSE fan-out consumer.  Errors here are logged but do
    // not prevent the HTTP server from starting — the SSE endpoint degrades
    // gracefully when Kafka is unavailable (no events; long-poll returns empty).
    let consumer_cfg = ConsumerCfg::new("control-api-sse");
    match ingest_consumer::spawn(
        &consumer_cfg,
        sse_bus,
        Arc::new(state.pool.clone()),
        kafka_consistency,
    ) {
        Ok(_handle) => tracing::info!("ingest_consumer started"),
        Err(e) => tracing::warn!("ingest_consumer failed to start (Kafka unavailable?): {e}"),
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // SECURITY: Public and internal routes are served on separate listeners
    // to prevent internal endpoints from being exposed on the public interface.
    let public_app = routes::build_public(state.clone())
        .route(
            "/metrics",
            get(move || async move { metrics_handle.render() }),
        )
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(cors)
        .layer(axum::middleware::from_fn(
            middleware::otel_trace::otel_trace_middleware,
        ));

    let internal_app = routes::build_internal(state)
        .layer(TraceLayer::new_for_http())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(axum::middleware::from_fn(
            middleware::otel_trace::otel_trace_middleware,
        ));

    let public_addr: std::net::SocketAddr = config.listen_addr.parse()?;
    let internal_addr: std::net::SocketAddr = config.internal_listen_addr.parse()?;

    tracing::info!(addr = %public_addr, "control-api public listener binding");
    tracing::info!(addr = %internal_addr, "control-api internal listener binding");

    let public_listener = tokio::net::TcpListener::bind(public_addr).await?;
    let internal_listener = tokio::net::TcpListener::bind(internal_addr).await?;

    // Spawn both servers concurrently; shutdown_signal triggers graceful shutdown for both.
    let public_server =
        axum::serve(public_listener, public_app).with_graceful_shutdown(shutdown_signal());
    let internal_server =
        axum::serve(internal_listener, internal_app).with_graceful_shutdown(shutdown_signal());

    tokio::try_join!(public_server, internal_server)?;

    Ok(())
}

fn build_producer<P, Err: std::fmt::Display>(result: Result<P, Err>, name: &str) -> Option<Arc<P>> {
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

/// Resolve the active GitHub App at startup using the Phase 2 source-priority
/// rules: prefer the singleton-active row in `control.github_app_config` (if
/// `RB_GH_APP_ENC_KEY` is configured), fall back to the legacy env-var path,
/// otherwise return `None` (GitHub routes 503).
///
/// Fails fast on any malformed credential — operator mistakes surface at boot,
/// not at first API call.
async fn resolve_gh_app(config: &Config, pool: &sqlx::PgPool) -> Result<Option<GhApp>> {
    // 1. DB-first path. Only attempted when an encryption key is configured;
    //    without one we cannot decrypt stored secrets even if a row exists.
    if let Some(key_b64) = config.gh_app_enc_key_b64.as_deref() {
        let key = EncryptionKey::from_base64(key_b64)
            .context("RB_GH_APP_ENC_KEY: invalid AES-256-GCM key material")?;
        let store = AppConfigStore::new(pool.clone(), key);
        match store.load_active().await {
            Ok(Some(cfg)) => {
                let app = try_build_gh_app(&cfg)
                    .context("active control.github_app_config row could not be hydrated")?;
                tracing::info!(
                    app_id = cfg.app_id,
                    slug = %cfg.slug,
                    source = "db",
                    "GitHub App loaded from control.github_app_config"
                );
                return Ok(Some(app));
            }
            Ok(None) => {
                tracing::info!(
                    "RB_GH_APP_ENC_KEY set but no active github_app_config row — falling back to env"
                );
            }
            Err(e) => {
                // Surface as boot failure: if the store is reachable but a
                // row is corrupted, we do not want to silently fall through
                // to env vars and confuse the operator.
                return Err(
                    anyhow::anyhow!(e).context("failed to load active github_app_config row")
                );
            }
        }
    }

    // 2. Legacy env-var path (unchanged semantics from Phase 1).
    let (Some(app_id), Some(pem_b64)) =
        (config.gh_app_id, config.gh_app_private_key_b64.as_deref())
    else {
        tracing::info!(
            "RB_GH_APP_ID / RB_GH_APP_PRIVATE_KEY not set and no DB row — GitHub App disabled"
        );
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

    tracing::info!(app_id, source = "env", "GitHub App loaded from env vars");
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
