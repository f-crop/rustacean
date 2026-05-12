//! Self-service GitHub App registration via the Manifest flow.
//!
//! Three endpoints, all gated by [`crate::middleware::platform_admin::require_platform_admin`]:
//!
//! - [`post_app_manifest`] mints a state token and returns the GitHub
//!   redirect URL with the manifest blob.
//! - [`get_app_callback`] receives `code` + `state` from GitHub, exchanges
//!   `code` for App credentials, persists them, and hot-swaps the loader.
//! - [`get_app_status`] returns the current App configuration source.

pub mod app_callback;
pub mod app_manifest;
pub mod app_status;

pub use app_callback::get_app_callback;
pub use app_manifest::post_app_manifest;
pub use app_status::get_app_status;
