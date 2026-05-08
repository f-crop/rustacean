use uuid::Uuid;

mod ingest {
    #![allow(clippy::all, clippy::pedantic, dead_code)]
    include!(concat!(env!("OUT_DIR"), "/rust_brain.v1.rs"));
}

pub use ingest::{
    AuditEvent, EmbeddingPendingEvent, ExpandedFileEvent, GraphRelationEvent, IngestRequest,
    IngestStage, IngestStatus, IngestStatusEvent, ItemKind, ParsedItemEvent, RelationKind,
    SourceFileEvent, Tombstone, TypecheckedItemEvent,
    expanded_file_event, parsed_item_event, source_file_event, typechecked_item_event,
    // Agent types (ADR-009 Option B)
    AgentEvent, AgentEventKind, AgentErrorCategory, AgentRuntime,
    AgentSessionCommand, AgentSessionInput, AgentSessionStart, AgentSessionTerminate,
    agent_session_command,
};

/// Newtype over [`Uuid`] representing a tenant identifier.
/// Prost-generated event types are re-exported from this crate (see [`IngestRequest`] etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct TenantId(Uuid);

impl TenantId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    #[must_use]
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl From<Uuid> for TenantId {
    fn from(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl std::str::FromStr for TenantId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.parse::<Uuid>()?))
    }
}

impl Default for TenantId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_id_roundtrip_json() {
        let id = TenantId::new();
        let json = serde_json::to_string(&id).unwrap();
        let decoded: TenantId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, decoded);
    }

    #[test]
    fn tenant_id_display_matches_uuid() {
        let id = TenantId::new();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }

    #[test]
    fn ingest_request_fields_accessible() {
        let req = IngestRequest {
            tenant_id: "tenant-123".to_string(),
            event_id: "evt-456".to_string(),
            source: "github".to_string(),
            payload: vec![1, 2, 3],
            created_at_ms: 1_700_000_000_000,
            repo_id: "repo-uuid".to_string(),
            ingest_run_id: "run-uuid".to_string(),
            commit_sha: "abc123".to_string(),
            branch: "main".to_string(),
        };
        assert_eq!(req.source, "github");
        assert_eq!(req.payload, vec![1u8, 2, 3]);
        assert_eq!(req.repo_id, "repo-uuid");
    }

    #[test]
    fn ingest_status_roundtrip() {
        let status = IngestStatus::Done;
        let as_i32 = status as i32;
        let back = IngestStatus::try_from(as_i32).unwrap();
        assert_eq!(status, back);
    }

    #[test]
    fn ingest_status_unspecified_is_zero() {
        assert_eq!(IngestStatus::Unspecified as i32, 0);
    }

    #[test]
    fn ingest_status_event_fields_accessible() {
        let ev = IngestStatusEvent {
            ingest_request_id: "req-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            status: IngestStatus::Processing as i32,
            error_message: String::new(),
            occurred_at_ms: 1_700_000_001_000,
            stage: IngestStage::Clone as i32,
            stage_seq: 1,
            ingest_run_id: "run-uuid".to_string(),
            attempt: 0,
        };
        assert_eq!(ev.status, IngestStatus::Processing as i32);
        assert!(ev.error_message.is_empty());
        assert_eq!(ev.stage, IngestStage::Clone as i32);
    }

    #[test]
    fn audit_event_fields_accessible() {
        let ev = AuditEvent {
            schema_version: "rust_brain.v1".to_string(),
            event_id: "evt-789".to_string(),
            tenant_id: "tenant-1".to_string(),
            action: "ingest.stage.started".to_string(),
            outcome: "success".to_string(),
            ..Default::default()
        };
        assert_eq!(ev.schema_version, "rust_brain.v1");
        assert_eq!(ev.action, "ingest.stage.started");
    }

    #[test]
    fn ingest_stage_nine_variants() {
        let stages = [
            IngestStage::Clone,
            IngestStage::Expand,
            IngestStage::Parse,
            IngestStage::Typecheck,
            IngestStage::Extract,
            IngestStage::Embed,
            IngestStage::ProjectPg,
            IngestStage::ProjectNeo4j,
            IngestStage::ProjectQdrant,
        ];
        assert_eq!(stages.len(), 9);
    }

    #[test]
    fn ingest_stage_unspecified_is_zero() {
        assert_eq!(IngestStage::Unspecified as i32, 0);
    }

    #[test]
    fn pipeline_source_file_event_accessible() {
        let ev = SourceFileEvent {
            ingest_run_id: "run-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            repo_id: "repo-1".to_string(),
            relative_path: "src/main.rs".to_string(),
            sha256: "abc".to_string(),
            size_bytes: 1024,
            emitted_at_ms: 0,
            body: None,
        };
        assert_eq!(ev.relative_path, "src/main.rs");
    }

    #[test]
    fn tombstone_accessible() {
        let t = Tombstone {
            tenant_id: "t".to_string(),
            repo_id: "r".to_string(),
            requested_by: "u".to_string(),
            emitted_at_ms: 0,
        };
        assert_eq!(t.tenant_id, "t");
    }
}
