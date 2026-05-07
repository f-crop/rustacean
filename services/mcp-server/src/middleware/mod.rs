use axum::{
    extract::Request,
    middleware::Next,
    response::Response,
};
use tracing::Instrument;

pub mod auth;

pub use auth::api_key_auth_middleware;

pub async fn otel_trace_middleware(req: Request, next: Next) -> Response {
    let span = tracing::info_span!(
        "http_request",
        method = %req.method(),
        uri = %req.uri(),
    );

    next.run(req).instrument(span).await
}
