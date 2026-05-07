use axum::{Router, routing::{get, post}};

use crate::state::AppState;

pub mod mcp;

pub use mcp::mcp_post_handler as mcp_handler;

pub fn build(state: AppState) -> Router {
    Router::new()
        .route("/mcp", post(mcp_handler))
        .route("/health", get(health_handler))
        .with_state(state)
}

async fn health_handler() -> &'static str {
    "ok"
}
