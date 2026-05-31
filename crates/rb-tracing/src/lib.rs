mod json_layer;

pub use json_layer::StructuredJsonLayer;

/// Returns the W3C 32-hex trace ID of the currently entered tracing span,
/// or `None` when no active OpenTelemetry span context is available.
///
/// Reads from the current tracing span's extensions via `OpenTelemetrySpanExt`
/// rather than `opentelemetry::Context::current()`, which is never populated
/// by `tracing-opentelemetry` 0.29.
#[must_use]
pub fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let otel_ctx = tracing::Span::current().context();
    let otel_span = otel_ctx.span();
    let sc = otel_span.span_context();
    sc.is_valid().then(|| sc.trace_id().to_string())
}

/// Returns the W3C 32-hex trace ID from `span`'s `OTel` context, or `None` when
/// the span has no valid OpenTelemetry context (e.g. no `OpenTelemetryLayer` installed).
///
/// Prefer this over [`current_trace_id`] when the span object is available
/// before it has been entered, e.g. in HTTP middleware that captures the trace ID
/// before driving the handler future.
#[must_use]
pub fn span_trace_id(span: &tracing::Span) -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    let otel_ctx = span.context();
    let otel_span = otel_ctx.span();
    let sc = otel_span.span_context();
    sc.is_valid().then(|| sc.trace_id().to_string())
}

use opentelemetry::{global, trace::TracerProvider as _};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, SpanExporter, WithExportConfig as _};
use opentelemetry_sdk::{
    Resource, logs::SdkLoggerProvider, propagation::TraceContextPropagator,
    trace::SdkTracerProvider,
};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _};

#[derive(Debug, thiserror::Error)]
pub enum TracingError {
    #[error("failed to initialize OTLP exporter: {0}")]
    OtlpInit(String),
    #[error("failed to set global subscriber: {0}")]
    Subscriber(String),
}

/// Flushes pending spans and log records on drop. Hold for the process lifetime.
pub struct TracingGuard {
    tracer_provider: SdkTracerProvider,
    logger_provider: SdkLoggerProvider,
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            eprintln!("tracer provider shutdown failed: {e}");
        }
        if let Err(e) = self.logger_provider.shutdown() {
            eprintln!("logger provider shutdown failed: {e}");
        }
    }
}

/// Initialize OTLP tracing, log export, and structured logging.
///
/// Installs a W3C `TraceContext` propagator, a batched OTLP gRPC span exporter,
/// a batched OTLP gRPC log exporter (bridged via `opentelemetry-appender-tracing`),
/// and a `tracing-subscriber` registry with JSON (production) or `pretty`
/// (dev) formatting. Call once at binary startup before any `tracing` macros.
///
/// Configuration via env vars:
/// - `OTEL_EXPORTER_OTLP_ENDPOINT` — gRPC endpoint (default: `http://localhost:4317`)
/// - `RUST_LOG`                     — log filter (default: `info`)
/// - `RB_LOG_FORMAT`                — `json` (default) or `pretty`
///
/// # Errors
///
/// Returns [`TracingError::OtlpInit`] if the OTLP exporter fails to build.
/// Returns [`TracingError::Subscriber`] if a global subscriber is already set.
pub fn init(service_name: &str) -> Result<TracingGuard, TracingError> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .unwrap_or_else(|_| "http://localhost:4317".to_string());

    let resource = Resource::builder()
        .with_service_name(service_name.to_owned())
        .build();

    // Trace pipeline
    let span_exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| TracingError::OtlpInit(e.to_string()))?;

    let tracer_provider = SdkTracerProvider::builder()
        .with_batch_exporter(span_exporter)
        .with_resource(resource.clone())
        .build();

    global::set_tracer_provider(tracer_provider.clone());

    // Log pipeline — bridges tracing events to OTLP log records routed to Loki
    let log_exporter = LogExporter::builder()
        .with_tonic()
        .with_endpoint(&endpoint)
        .build()
        .map_err(|e| TracingError::OtlpInit(e.to_string()))?;

    let logger_provider = SdkLoggerProvider::builder()
        .with_batch_exporter(log_exporter)
        .with_resource(resource)
        .build();

    let log_bridge = OpenTelemetryTracingBridge::new(&logger_provider);

    let log_format = std::env::var("RB_LOG_FORMAT").unwrap_or_else(|_| "json".to_string());

    if log_format == "pretty" {
        tracing_subscriber::registry()
            .with(EnvFilter::from_default_env())
            .with(fmt::layer().pretty())
            .with(OpenTelemetryLayer::new(
                tracer_provider.tracer("rb-tracing"),
            ))
            .with(log_bridge)
            .try_init()
            .map_err(|e| TracingError::Subscriber(e.to_string()))?;
    } else {
        tracing_subscriber::registry()
            .with(EnvFilter::from_default_env())
            .with(StructuredJsonLayer::stdout())
            .with(OpenTelemetryLayer::new(
                tracer_provider.tracer("rb-tracing"),
            ))
            .with(log_bridge)
            .try_init()
            .map_err(|e| TracingError::Subscriber(e.to_string()))?;
    }

    Ok(TracingGuard {
        tracer_provider,
        logger_provider,
    })
}
