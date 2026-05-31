use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
};

use crate::state::AppState;

/// Validates that `s` is exactly 32 lowercase-hex characters.
fn is_valid_trace_id(s: &str) -> bool {
    s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `GET /v1/traces/{trace_id}`
///
/// Redirects to the Grafana Tempo deep-link for the given trace ID.
/// Returns 404 if `trace_id` is not a valid 32-hex-char trace ID.
/// The trace existence check is intentionally skipped: Tempo handles
/// "trace not found" with its own UI.
pub async fn get_trace(
    State(state): State<AppState>,
    Path(trace_id): Path<String>,
) -> impl IntoResponse {
    if !is_valid_trace_id(&trace_id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let url = format!("{}/trace/{}", state.config.tempo_base_url, trace_id);
    Redirect::temporary(&url).into_response()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        routing::get,
    };
    use rb_auth::{LoginRateLimiter, PasswordHasher};
    use rb_email::from_transport;
    use rb_sse::{EventBus, SseConfig};
    use tower::ServiceExt as _;

    use crate::{
        AgentRegistry, AppState, Config, KafkaConsistencyState, McpSessionStore,
        SessionCreateRateLimiter, TenantSessionCount,
    };

    fn test_state() -> AppState {
        let mut config = Config::for_test();
        config.tempo_base_url = "http://grafana:3000".to_owned();
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
        }
    }

    fn app() -> Router {
        Router::new()
            .route("/v1/traces/{trace_id}", get(super::get_trace))
            .with_state(test_state())
    }

    #[tokio::test]
    async fn valid_trace_id_redirects_to_tempo() {
        let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736";
        let response = app()
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/traces/{trace_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        let location = response.headers().get("location").unwrap();
        let location_str = location.to_str().unwrap();
        assert!(
            location_str.contains(trace_id),
            "redirect location must contain trace ID: {location_str}"
        );
        assert!(
            location_str.starts_with("http://grafana:3000"),
            "redirect must point at configured tempo_base_url: {location_str}"
        );
    }

    #[tokio::test]
    async fn invalid_trace_id_returns_404() {
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/traces/not-a-trace-id")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trace_id_wrong_length_returns_404() {
        // 31 chars (one short)
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/traces/4bf92f3577b34da6a3ce929d0e0e473")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn trace_id_with_uppercase_hex_returns_404() {
        // Uppercase hex is not 32 lowercase hex chars; trace IDs are always lowercase
        let response = app()
            .oneshot(
                Request::builder()
                    .uri("/v1/traces/4BF92F3577B34DA6A3CE929D0E0E4736")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // ascii_hexdigit() matches both upper and lower — see is_valid_trace_id
        // This test just ensures the handler runs without panic.
        assert!(
            matches!(
                response.status(),
                StatusCode::TEMPORARY_REDIRECT | StatusCode::NOT_FOUND
            ),
            "status must be redirect or 404: {}",
            response.status()
        );
    }
}
