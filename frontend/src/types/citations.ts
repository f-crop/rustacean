// TEMPORARY: Hand-written mirror of rb-schemas::CitationV1 and friends.
// Matches crates/rb-schemas/src/citation.rs exactly.
// Replace this file with a codegen'd import from @/api/generated/schema
// after RUSAA-2089 lands and `npm run gen:api` regenerates the OpenAPI schema.

export type SourceKind = "dense" | "sparse" | "hybrid" | "rerank";

export interface LineRange {
  readonly start: number;
  readonly end: number;
}

/** ADR-014 §5 citation envelope — frozen at v1. */
export interface CitationV1 {
  readonly version: string;
  readonly repo_id: string;
  readonly file_path: string;
  readonly line_range: LineRange;
  /** Best-effort "ingested at this commit" SHA; always non-empty (may be "unknown"). */
  readonly commit_sha: string;
  /** Fused score normalised to [0, 1]. */
  readonly score: number;
  readonly source_kind: SourceKind;
}
