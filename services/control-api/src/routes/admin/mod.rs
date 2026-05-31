//! Platform-admin routes (gated by `require_platform_admin`).
//!
//! These endpoints register or manage deployment-wide resources — they are
//! intentionally separate from per-tenant administration (`tenants/role.rs`).

pub mod github;
pub mod partition_maintenance;
pub mod v1;
