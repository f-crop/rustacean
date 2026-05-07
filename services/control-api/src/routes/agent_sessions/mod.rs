use axum::{extract::{Path, State}, http::HeaderMap, response::IntoResponse, Json};
use rb_kafka::Producer;
use rb_schemas::{AgentCommand, AgentRuntime, SessionStart, SessionTerminate, TenantId};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    error::AppError,
    middleware::auth::{require_session, AuthContext},
    routes::agent_sessions::{
        create::create_session_handler,
        delete::delete_session_handler,
        events::session_events_handler,
        get::get_session_handler,
    },
    state::AppState,
};

pub mod create;
pub mod delete;
pub mod events;
pub mod get;

pub use create::create_session;
pub use delete::delete_session;
pub use events::session_events;
pub use get::get_session;
