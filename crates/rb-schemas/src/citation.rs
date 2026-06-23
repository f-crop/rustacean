use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Line range (inclusive on both ends) within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LineRange {
    pub start: i32,
    pub end: i32,
}

/// Which retrieval leg(s) produced this hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Dense,
    Sparse,
    Hybrid,
    Rerank,
}

/// Versioned citation envelope returned by `/v1/search` and `search_items` MCP tool
/// when `RB_HYBRID_SEARCH_ENABLED=true` (ADR-014 §5, Wave 10 S2).
///
/// S4 (chat UI citation rendering) builds against this exact shape. Field additions
/// within the same `version` value are additive; breaking changes must bump `version`.
///
/// `commit_sha` is always non-empty: Wave 10 sources the repo-level head SHA at
/// ingest time when per-symbol SHAs are unavailable. Consumers should treat it as
/// a best-effort "ingested at this commit" rather than a per-line provenance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CitationV1 {
    /// Schema version tag. Always `"v1"` for this type.
    pub version: String,
    /// Repository that contains this symbol.
    pub repo_id: Uuid,
    /// Relative path within the repository (from `code_symbols.source_path`).
    pub file_path: String,
    /// Source line range (from `code_symbols.line_start`/`line_end`).
    pub line_range: LineRange,
    /// Commit SHA at which this symbol was ingested. Non-empty; see type-level docs.
    pub commit_sha: String,
    /// Fused score normalized to `[0, 1]`.
    pub score: f32,
    /// Which retrieval path(s) produced this result.
    pub source_kind: SourceKind,
}

impl CitationV1 {
    pub const VERSION: &'static str = "v1";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn citation_v1_serializes_version_field() {
        let c = CitationV1 {
            version: CitationV1::VERSION.to_owned(),
            repo_id: Uuid::nil(),
            file_path: "src/lib.rs".to_owned(),
            line_range: LineRange { start: 1, end: 10 },
            commit_sha: "abc123".to_owned(),
            score: 0.85,
            source_kind: SourceKind::Hybrid,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["version"], "v1");
        assert_eq!(json["source_kind"], "hybrid");
        assert_eq!(json["line_range"]["start"], 1);
        assert_eq!(json["line_range"]["end"], 10);
    }

    #[test]
    fn citation_v1_roundtrip() {
        let c = CitationV1 {
            version: CitationV1::VERSION.to_owned(),
            repo_id: Uuid::new_v4(),
            file_path: "crates/rb-query/src/lib.rs".to_owned(),
            line_range: LineRange { start: 42, end: 87 },
            commit_sha: "deadbeef12345678".to_owned(),
            score: 0.73,
            source_kind: SourceKind::Dense,
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: CitationV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file_path, c.file_path);
        assert_eq!(back.line_range.start, c.line_range.start);
        assert_eq!(back.commit_sha, c.commit_sha);
        assert_eq!(back.source_kind, SourceKind::Dense);
    }

    #[test]
    fn source_kind_all_variants_serialize() {
        for (kind, expected) in [
            (SourceKind::Dense, "dense"),
            (SourceKind::Sparse, "sparse"),
            (SourceKind::Hybrid, "hybrid"),
            (SourceKind::Rerank, "rerank"),
        ] {
            let s = serde_json::to_value(kind).unwrap();
            assert_eq!(s, expected);
        }
    }
}
