/// Integration tests for `X-Trace-Id` response header surfacing (ADR-012 §S4).
///
/// These tests verify that every HTTP response from control-api carries the
/// `X-Trace-Id` header when the `OTel` tracing stack is initialised, covering
/// the round-trip requirement from ADR-012 §2.4.4 (S5 CI gate).
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    middleware,
};
use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_sdk::{propagation::TraceContextPropagator, trace::SdkTracerProvider};
use std::sync::OnceLock;
use tower::ServiceExt as _;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use control_api::{
    AgentRegistry, AppState, Config, KafkaConsistencyState, McpSessionStore,
    SessionCreateRateLimiter, TenantSessionCount, build_public,
};
use rb_auth::{LoginRateLimiter, PasswordHasher};
use rb_email::from_transport;
use rb_sse::{EventBus, SseConfig};

static OTEL_INIT: OnceLock<()> = OnceLock::new();

/// Initialises a minimal in-process `OTel` tracer (no OTLP exporter) so that
/// `OpenTelemetryLayer` is installed and `span_trace_id` returns a valid ID.
fn init_test_otel() {
    OTEL_INIT.get_or_init(|| {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let provider = SdkTracerProvider::builder().build();
        global::set_tracer_provider(provider.clone());
        let _ = tracing_subscriber::registry()
            .with(OpenTelemetryLayer::new(provider.tracer("test")))
            .try_init();
    });
}

fn test_state() -> AppState {
    let config = Config::for_test();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&config.database_url)
        .expect("connect_lazy must succeed");
    let smtp = rb_email::SmtpConfig {
        host: String::new(),
        port: 587,
        username: String::new(),
        password: String::new(),
        from_address: "test@example.com".to_owned(),
    };
    let email_sender = from_transport("noop", &smtp).expect("noop transport must succeed");
    let hasher = PasswordHasher::from_config(64, 1, 1).expect("hasher must build");
    AppState {
        pool,
        email_sender: Arc::from(email_sender),
        hasher: Arc::new(hasher),
        login_rate_limiter: Arc::new(LoginRateLimiter::new()),
        config: Arc::new(config),
        gh_loader: Arc::new(rb_github::GhAppLoader::new(None)),
        sse_bus: Arc::new(EventBus::new(SseConfig::default())),
        ingest_producer: None,
        tombstone_producer: None,
        module_tree_cache: rb_query::new_module_tree_cache(),
        graph: None,
        qdrant: None,
        http_client: reqwest::Client::new(),
        neo4j_uri: None,
        kafka_consistency: Arc::new(KafkaConsistencyState::new()),
        mcp_sessions: McpSessionStore::new(),
        agent_registry: AgentRegistry::new(),
        agent_commands_producer: None,
        internal_secret: "test-internal-secret".to_owned(),
        session_create_rate_limiter: Arc::new(SessionCreateRateLimiter::new(10, 60)),
        tenant_session_count: Arc::new(TenantSessionCount::new()),
        mcp_jwt_secret: "test-mcp-jwt-secret".to_owned(),
        mcp_jwt_ttl_secs: 900,
        llm_api_key: String::new(),
    }
}

/// Build an app that includes the `otel_trace_middleware` in the same position
/// as the real server, so the round-trip can be exercised in unit tests.
fn app_with_middleware() -> Router {
    build_public(test_state()).layer(middleware::from_fn(control_api::otel_trace_middleware))
}

// ── Header round-trip tests ────────────────────────────────────────────────

#[tokio::test]
async fn x_trace_id_present_on_health_response() {
    init_test_otel();

    let response = app_with_middleware()
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let header = response.headers().get("x-trace-id");
    assert!(
        header.is_some(),
        "x-trace-id header must be present on every response when OTel is active"
    );
    let value = header.unwrap().to_str().unwrap();
    assert_eq!(
        value.len(),
        32,
        "trace ID must be 32 hex chars, got: {value}"
    );
    assert!(
        value.chars().all(|c| c.is_ascii_hexdigit()),
        "trace ID must be hex, got: {value}"
    );
}

#[tokio::test]
async fn x_trace_id_reuses_incoming_traceparent() {
    init_test_otel();

    let parent_trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";

    let response = app_with_middleware()
        .oneshot(
            Request::builder()
                .uri("/health")
                .header(
                    "traceparent",
                    format!("00-{parent_trace_id}-00f067aa0ba902b7-01"),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let header = response.headers().get("x-trace-id");
    assert!(
        header.is_some(),
        "x-trace-id must be present when traceparent was supplied"
    );
    assert_eq!(
        header.unwrap().to_str().unwrap(),
        parent_trace_id,
        "x-trace-id must echo the trace ID from the incoming traceparent header"
    );
}

#[tokio::test]
async fn x_trace_id_present_on_non_200_responses() {
    init_test_otel();

    let response = app_with_middleware()
        .oneshot(
            Request::builder()
                .uri("/v1/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // /v1/me requires auth — should 401
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let header = response.headers().get("x-trace-id");
    assert!(
        header.is_some(),
        "x-trace-id must be present even on error responses"
    );
}
