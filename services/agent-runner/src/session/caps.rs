//! Per-tenant and per-node concurrent session caps (ADR-013 §4.3).

use std::collections::HashMap;
use std::sync::Arc;

use metrics::counter;
use rb_schemas::TenantId;
use tokio::sync::{Mutex, Semaphore};

pub(super) const MAX_SESSIONS_PER_TENANT: usize = 20;
const MAX_SESSIONS_PER_NODE: usize = 200;

pub struct SessionCaps {
    tenant_counts: Arc<Mutex<HashMap<TenantId, usize>>>,
    node_semaphore: Arc<Semaphore>,
}

impl SessionCaps {
    pub fn new() -> Self {
        Self {
            tenant_counts: Arc::new(Mutex::new(HashMap::new())),
            node_semaphore: Arc::new(Semaphore::new(MAX_SESSIONS_PER_NODE)),
        }
    }

    pub fn tenant_counts(&self) -> Arc<Mutex<HashMap<TenantId, usize>>> {
        Arc::clone(&self.tenant_counts)
    }

    /// Acquire node + tenant permits. Returns `Err` if either cap is exceeded.
    pub async fn acquire(
        &self,
        tenant_id: TenantId,
    ) -> anyhow::Result<tokio::sync::OwnedSemaphorePermit> {
        let permit = Arc::clone(&self.node_semaphore)
            .try_acquire_owned()
            .map_err(|_| {
                counter!("rb_session_rejected_total", "reason" => "node_limit").increment(1);
                anyhow::anyhow!(
                    "error_kind=rate_limit_exceeded: node session limit ({MAX_SESSIONS_PER_NODE}) reached"
                )
            })?;
        let mut counts = self.tenant_counts.lock().await;
        let n = counts.entry(tenant_id).or_insert(0);
        if *n >= MAX_SESSIONS_PER_TENANT {
            counter!("rb_session_rejected_total", "reason" => "tenant_limit").increment(1);
            anyhow::bail!(
                "error_kind=rate_limit_exceeded: tenant session limit ({MAX_SESSIONS_PER_TENANT}) reached"
            );
        }
        *n += 1;
        Ok(permit)
    }

    pub async fn release(&self, tenant_id: TenantId) {
        let mut counts = self.tenant_counts.lock().await;
        if let Some(n) = counts.get_mut(&tenant_id) {
            *n = n.saturating_sub(1);
        }
    }
}
