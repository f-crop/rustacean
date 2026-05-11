//! Agent execution routes (ADR-009 Option B — process-spawning via rb-agent-runner).

pub mod events;
pub mod session_lifecycle;
pub mod session_queries;
pub mod sessions;

pub use events::session_events;
pub use session_queries::{get_session, list_sessions};
pub use sessions::{create_session, delete_session, delete_session_api_key, patch_session_status};
