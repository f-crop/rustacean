//! Chat panel routes — ADR-013 §3 (Wave 9 S3).
//!
//! All routes return 404 when `RB_CHAT_PANEL_ENABLED` is false (feature gate).

mod db;
pub mod events;
pub mod messages;
pub mod sessions;

pub use events::chat_session_events;
pub use messages::{list_chat_messages, post_chat_message};
pub use sessions::{create_chat_session, get_chat_session};

#[cfg(test)]
mod tests;
