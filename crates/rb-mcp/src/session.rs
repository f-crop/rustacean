//! In-process MCP session store (ADR-009 Phase 1).
//!
//! Keyed by the opaque session UUID returned in `Mcp-Session-Id`. The value
//! is the `tenant_id` bound at `initialize` time and is IMMUTABLE: every
//! `tools/call` rejects a mismatched auth tenant (`TENANT_DRIFT -32000`).
//!
//! Session eviction is not implemented in Phase 1; Phase 2 adds an idle-timeout reaper.

use std::sync::Arc;

use dashmap::DashMap;
use uuid::Uuid;

/// Thread-safe in-memory MCP session table.
///
/// Each session maps a random UUID session ID → bound `tenant_id`. The store
/// is cheap to clone (Arc-wrapped internally) and can be placed in a shared `AppState`.
#[derive(Clone, Default)]
pub struct McpSessionStore(Arc<DashMap<Uuid, Uuid>>);

impl McpSessionStore {
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(DashMap::new()))
    }

    /// Register a new session bound to `tenant_id` and return its UUID.
    #[must_use]
    pub fn create(&self, tenant_id: Uuid) -> Uuid {
        let session_id = Uuid::new_v4();
        self.0.insert(session_id, tenant_id);
        session_id
    }

    /// Return the `tenant_id` for the given `session_id`, or `None` if unknown/evicted.
    #[must_use]
    pub fn tenant_id(&self, session_id: &Uuid) -> Option<Uuid> {
        self.0.get(session_id).map(|r| *r)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_returns_distinct_ids() {
        let store = McpSessionStore::new();
        let tid = Uuid::new_v4();
        assert_ne!(store.create(tid), store.create(tid));
    }

    #[test]
    fn tenant_id_round_trips() {
        let store = McpSessionStore::new();
        let tid = Uuid::new_v4();
        let sid = store.create(tid);
        assert_eq!(store.tenant_id(&sid), Some(tid));
    }

    #[test]
    fn unknown_session_returns_none() {
        let store = McpSessionStore::new();
        assert_eq!(store.tenant_id(&Uuid::new_v4()), None);
    }

    #[test]
    fn clone_shares_same_backing_store() {
        let store = McpSessionStore::new();
        let tid = Uuid::new_v4();
        let sid = store.create(tid);
        assert_eq!(store.clone().tenant_id(&sid), Some(tid));
    }
}
