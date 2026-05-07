use std::sync::Arc;

use anyhow::{Context as _, Result};
use rb_storage_neo4j::TenantGraph;
use rb_storage_qdrant::TenantVectorStore;
use sqlx::postgres::PgPoolOptions;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

use crate::{
    config::Config,
    middleware::{api_key_auth_middleware, otel_trace_middleware},
    routes,
    state::AppState,
};

#[allow(clippy::missing_errors_doc)]
pub async fn run(config: Config) -> Result<()> {
    let metrics_handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .install_recorder()
        .context("failed to install Prometheus metrics recorder")?;

    let pool = PgPoolOptions::new()
        .max_connections(20)
        .connect(&config.database_url)
        .await
        .context("failed to connect to Postgres")?;

    let graph = if let (Some(uri), Some(password)) =
        (config.neo4j_uri.as_deref(), config.neo4j_password.as_deref())
    {
        match TenantGraph::connect(uri, &config.neo4j_user, password).await {
            Ok(g) => {
                tracing::info!("neo4j connected at {}", uri);
                Some(Arc::new(g))
            }
            Err(e) => {
                tracing::warn!("neo4j connection failed: {}", e);
                None
            }
        }
    } else {
        tracing::info!("RB_NEO4J_URI / RB_NEO4J_PASSWORD not set — graph endpoints disabled");
        None
    };

    let qdrant = config.qdrant_url.as_deref().map(|url| {
        tracing::info!("qdrant configured at {}", url);
        Arc::new(TenantVectorStore::new(url))
    });

    let mut state = AppState::new(pool, Arc::new(config.clone()));

    if let Some(g) = graph {
        state = state.with_graph(g);
    }
    if let Some(q) = qdrant {
        state = state.with_qdrant(q);
    }

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::build(state.clone())
        .route("/metrics", axum::routing::get(move || async move { metrics_handle.render() }))
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api_key_auth_middleware,
        ))
        .layer(axum::middleware::from_fn(otel_trace_middleware));

    let addr: std::net::SocketAddr = config.listen_addr.parse()?;
    tracing::info!(addr = %addr, "mcp-server listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

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
