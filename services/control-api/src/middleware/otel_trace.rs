use axum::{extract::Request, middleware::Next, response::Response};
use opentelemetry::propagation::Extractor;
use tracing::Instrument as _;

struct HttpHeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HttpHeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

/// Tower middleware that opens an `OTel` HTTP server span for every inbound request.
///
/// Extracts `traceparent`/`tracestate` W3C headers for distributed trace propagation,
/// mirroring the pattern in `rb-kafka/src/consumer.rs` consume-span construction.
/// The `tracing` span is bridged to OpenTelemetry by `tracing-opentelemetry`, so
/// `opentelemetry::Context::current()` is non-empty during handler execution and
/// `StructuredJsonLayer` emits a populated `trace_id`.
///
/// When no `traceparent` header is present a new root trace is started, so every
/// request — internal or external — carries a non-empty trace ID in its logs.
pub async fn otel_trace_middleware(request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().path().to_owned();

    // Extract W3C trace context from headers and create the tracing span while the
    // parent context is attached.  _cx_guard is scoped to span construction:
    // tracing-opentelemetry reads the current OTel context in on_new_span to wire
    // the parent→child relationship; the guard can be dropped immediately after.
    // This follows the same scoping used in rb-kafka/src/consumer.rs:107-132.
    let span = {
        let parent_cx = opentelemetry::global::get_text_map_propagator(|propagator| {
            propagator.extract(&HttpHeaderExtractor(request.headers()))
        });
        let _cx_guard = parent_cx.attach();
        tracing::info_span!(
            "http.request",
            "otel.kind" = "SERVER",
            "http.method" = %method,
            "http.target" = %uri,
        )
        // _cx_guard dropped here; span retains the parent relationship set at creation
    };

    next.run(request).instrument(span).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode},
        middleware, routing::get, Router,
    };
    use tower::ServiceExt as _;

    fn header_map_with(key: &'static str, value: &'static str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.insert(
            HeaderName::from_static(key),
            HeaderValue::from_static(value),
        );
        m
    }

    #[test]
    fn extractor_returns_traceparent() {
        let map = header_map_with(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        );
        let ext = HttpHeaderExtractor(&map);
        assert_eq!(
            ext.get("traceparent"),
            Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01")
        );
    }

    #[test]
    fn extractor_returns_tracestate() {
        let map = header_map_with("tracestate", "vendor=value");
        let ext = HttpHeaderExtractor(&map);
        assert_eq!(ext.get("tracestate"), Some("vendor=value"));
    }

    #[test]
    fn extractor_is_case_insensitive_via_http_crate() {
        // http::HeaderMap canonicalises names to lowercase on insert, so lookups
        // for "traceparent" and "TRACEPARENT" both hit the same slot.
        let map = header_map_with("traceparent", "val");
        let ext = HttpHeaderExtractor(&map);
        assert_eq!(ext.get("traceparent"), Some("val"));
        assert_eq!(ext.get("TRACEPARENT"), Some("val"));
        assert_eq!(ext.get("Traceparent"), Some("val"));
    }

    #[test]
    fn extractor_returns_none_for_absent_key() {
        let map = HeaderMap::new();
        let ext = HttpHeaderExtractor(&map);
        assert!(ext.get("traceparent").is_none());
    }

    #[test]
    fn extractor_keys_lists_header_names() {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_static("traceparent"),
            HeaderValue::from_static("v1"),
        );
        map.insert(
            HeaderName::from_static("tracestate"),
            HeaderValue::from_static("v2"),
        );
        let ext = HttpHeaderExtractor(&map);
        let keys = ext.keys();
        assert!(keys.contains(&"traceparent"));
        assert!(keys.contains(&"tracestate"));
    }

    #[tokio::test]
    async fn middleware_passes_request_through() {
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .layer(middleware::from_fn(otel_trace_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn middleware_passes_request_with_traceparent_header() {
        let app = Router::new()
            .route("/", get(|| async { "ok" }))
            .layer(middleware::from_fn(otel_trace_middleware));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/")
                    .header(
                        "traceparent",
                        "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
