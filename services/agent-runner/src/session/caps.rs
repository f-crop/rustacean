//! Per-tenant and per-node concurrent session caps (ADR-013 §4.3).

use std::collections::HashMap;
use std::sync::Arc;

use metrics::counter;
use rb_schemas::TenantId;
use tokio::sync::Semaphore;

pub(super) const MAX_SESSIONS_PER_TENANT: usize = 20;
const MAX_SESSIONS_PER_NODE: usize = 200;

pub struct SessionCaps {
    // std::sync::Mutex (not tokio) so TenantCountGuard::drop can release
    // the count synchronously without an async runtime.
    tenant_counts: Arc<std::sync::Mutex<HashMap<TenantId, usize>>>,
    node_semaphore: Arc<Semaphore>,
}

/// RAII guard that rolls back the per-tenant session counter on drop.
///
/// Returned alongside the node-semaphore permit by [`SessionCaps::acquire`].
/// During session setup, any early `?` return drops this guard and restores
/// the counter automatically. Call [`defuse`] once the session is committed
/// to the live session map; after that point [`terminate_session`] /
/// `natural_exit` handle the eventual decrement.
pub struct TenantCountGuard {
    tenant_counts: Arc<std::sync::Mutex<HashMap<TenantId, usize>>>,
    tenant_id: TenantId,
    armed: bool,
}

impl TenantCountGuard {
    /// Defuse the guard — the session is now live; the tenant counter will be
    /// decremented later by `caps.release()` or `natural_exit`.
    pub fn defuse(&mut self) {
        self.armed = false;
    }

    /// Create a pre-defused guard for use in test stubs that need a
    /// `SessionHandle` without going through a real `SessionCaps::acquire`.
    #[cfg(test)]
    pub(super) fn new_defused_for_test() -> Self {
        Self {
            tenant_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            tenant_id: TenantId::from(uuid::Uuid::nil()),
            armed: false,
        }
    }
}

impl Drop for TenantCountGuard {
    fn drop(&mut self) {
        if self.armed {
            if let Ok(mut counts) = self.tenant_counts.lock() {
                if let Some(n) = counts.get_mut(&self.tenant_id) {
                    *n = n.saturating_sub(1);
                }
            }
        }
    }
}

impl SessionCaps {
    pub fn new() -> Self {
        Self {
            tenant_counts: Arc::new(std::sync::Mutex::new(HashMap::new())),
            node_semaphore: Arc::new(Semaphore::new(MAX_SESSIONS_PER_NODE)),
        }
    }

    pub fn tenant_counts(&self) -> Arc<std::sync::Mutex<HashMap<TenantId, usize>>> {
        Arc::clone(&self.tenant_counts)
    }

    /// Acquire node + tenant permits.
    ///
    /// Returns `Err` if either cap is exceeded. On `Ok`, the caller receives:
    /// - an `OwnedSemaphorePermit` for the node-level semaphore (RAII)
    /// - a `TenantCountGuard` that rolls back the tenant counter on drop
    ///   unless [`TenantCountGuard::defuse`] is called first.
    pub fn acquire(
        &self,
        tenant_id: TenantId,
    ) -> anyhow::Result<(tokio::sync::OwnedSemaphorePermit, TenantCountGuard)> {
        let permit = Arc::clone(&self.node_semaphore)
            .try_acquire_owned()
            .map_err(|_| {
                counter!("rb_session_rejected_total", "reason" => "node_limit").increment(1);
                anyhow::anyhow!(
                    "error_kind=rate_limit_exceeded: node session limit ({MAX_SESSIONS_PER_NODE}) reached"
                )
            })?;
        let mut counts = self.tenant_counts.lock().unwrap();
        let n = counts.entry(tenant_id).or_insert(0);
        if *n >= MAX_SESSIONS_PER_TENANT {
            counter!("rb_session_rejected_total", "reason" => "tenant_limit").increment(1);
            anyhow::bail!(
                "error_kind=rate_limit_exceeded: tenant session limit ({MAX_SESSIONS_PER_TENANT}) reached"
            );
        }
        *n += 1;
        let guard = TenantCountGuard {
            tenant_counts: Arc::clone(&self.tenant_counts),
            tenant_id,
            armed: true,
        };
        Ok((permit, guard))
    }

    pub fn release(&self, tenant_id: TenantId) {
        let mut counts = self.tenant_counts.lock().unwrap();
        if let Some(n) = counts.get_mut(&tenant_id) {
            *n = n.saturating_sub(1);
        }
    }
}
