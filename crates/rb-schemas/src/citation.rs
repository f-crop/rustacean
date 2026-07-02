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
///
/// `fqn` and `crate_name` are populated when the source is a code symbol; `None`
/// for non-code sources (docs, markdown). Added additively (RUSAA-2177, Wave 10
/// realign) to restore the `search_items → get_item` chain for LLM callers.
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
    /// Fully-qualified name of the code symbol (e.g. `my_crate::MyStruct::method`).
    /// Populated for code-symbol sources; `None` for non-code sources (docs, markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fqn: Option<String>,
    /// Crate name derived from the leading `::` segment of `fqn`.
    /// Populated for code-symbol sources; `None` for non-code sources (docs, markdown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crate_name: Option<String>,
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
            fqn: Some("my_crate::MyStruct".to_owned()),
            crate_name: Some("my_crate".to_owned()),
        };
        let json = serde_json::to_value(&c).unwrap();
        assert_eq!(json["version"], "v1");
        assert_eq!(json["source_kind"], "hybrid");
        assert_eq!(json["line_range"]["start"], 1);
        assert_eq!(json["line_range"]["end"], 10);
        assert_eq!(json["fqn"], "my_crate::MyStruct");
        assert_eq!(json["crate_name"], "my_crate");
    }

    #[test]
    fn citation_v1_fqn_omitted_when_none() {
        let c = CitationV1 {
            version: CitationV1::VERSION.to_owned(),
            repo_id: Uuid::nil(),
            file_path: "README.md".to_owned(),
            line_range: LineRange { start: 1, end: 1 },
            commit_sha: "abc123".to_owned(),
            score: 0.5,
            source_kind: SourceKind::Dense,
            fqn: None,
            crate_name: None,
        };
        let json = serde_json::to_value(&c).unwrap();
        assert!(
            json.get("fqn").is_none(),
            "fqn must be omitted from JSON when None"
        );
        assert!(
            json.get("crate_name").is_none(),
            "crate_name must be omitted from JSON when None"
        );
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
            fqn: Some("rb_query::hybrid_search".to_owned()),
            crate_name: Some("rb_query".to_owned()),
        };
        let json = serde_json::to_string(&c).unwrap();
        let back: CitationV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file_path, c.file_path);
        assert_eq!(back.line_range.start, c.line_range.start);
        assert_eq!(back.commit_sha, c.commit_sha);
        assert_eq!(back.source_kind, SourceKind::Dense);
        assert_eq!(back.fqn.as_deref(), Some("rb_query::hybrid_search"));
        assert_eq!(back.crate_name.as_deref(), Some("rb_query"));
    }

    #[test]
    fn citation_v1_roundtrip_no_fqn_deserializes_as_none() {
        // Old serialized payloads without fqn/crate_name must still deserialize.
        let legacy = r#"{
            "version": "v1",
            "repo_id": "00000000-0000-0000-0000-000000000000",
            "file_path": "src/lib.rs",
            "line_range": {"start": 1, "end": 5},
            "commit_sha": "abc",
            "score": 0.8,
            "source_kind": "hybrid"
        }"#;
        let c: CitationV1 = serde_json::from_str(legacy).unwrap();
        assert!(c.fqn.is_none());
        assert!(c.crate_name.is_none());
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
