//! Agent execution routes (ADR-009 Option B — process-spawning via rb-agent-runner).

pub mod events;
pub mod sessions;

pub use events::session_events;
pub use sessions::{
    create_session, delete_session,
    patch_session_status, delete_session_api_key,
};
