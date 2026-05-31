//! Admin tenant-management endpoints (ADR-012 §S1.2):
//!
//! - `POST /api/admin/v1/tenants/:id/rebind-gh-install`
//! - `POST /api/admin/v1/tenants/:id/impersonate`
//! - `POST /api/admin/v1/tenants/:id/force-delete`

mod force_delete;
mod impersonate;
mod rebind;

pub use force_delete::force_delete;
pub use impersonate::impersonate;
pub use rebind::rebind_gh_install;
