import { describe, it, expect } from "vitest";
import type { CitationV1, SourceKind } from "@/types/citations";
import { buildGitHubUrl, extractCitationsFromItems } from "./citation-utils";

function makeCitation(source_kind: SourceKind, overrides?: Partial<CitationV1>): CitationV1 {
  return {
    version: "v1",
    repo_id: "00000000-0000-0000-0000-000000000001",
    file_path: "src/lib.rs",
    line_range: { start: 10, end: 20 },
    commit_sha: "abc123def456",
    score: 0.876,
    source_kind,
    ...overrides,
  };
}

// ---------------------------------------------------------------------------
// CitationV1 type contract — all four source_kind variants
// ---------------------------------------------------------------------------

const ALL_KINDS: SourceKind[] = ["dense", "sparse", "hybrid", "rerank"];

describe("CitationV1 fixture round-trip", () => {
  it.each(ALL_KINDS)("preserves all fields for source_kind=%s", (kind) => {
    const c = makeCitation(kind);
    const rt: CitationV1 = JSON.parse(JSON.stringify(c));
    expect(rt.version).toBe("v1");
    expect(rt.source_kind).toBe(kind);
    expect(rt.repo_id).toBe(c.repo_id);
    expect(rt.file_path).toBe(c.file_path);
    expect(rt.line_range.start).toBe(10);
    expect(rt.line_range.end).toBe(20);
    expect(rt.commit_sha).toBe(c.commit_sha);
    expect(rt.score).toBeCloseTo(0.876, 3);
  });

  it("score formats to 2 decimal places", () => {
    expect(makeCitation("dense", { score: 0.9321 }).score.toFixed(2)).toBe("0.93");
    expect(makeCitation("rerank", { score: 0.1000 }).score.toFixed(2)).toBe("0.10");
    expect(makeCitation("hybrid", { score: 1 }).score.toFixed(2)).toBe("1.00");
  });

  it("detects version mismatch", () => {
    const c = makeCitation("sparse", { version: "v2" });
    expect(c.version).not.toBe("v1");
  });
});

// ---------------------------------------------------------------------------
// buildGitHubUrl
// ---------------------------------------------------------------------------

describe("buildGitHubUrl", () => {
  it("builds correct GitHub blob URL", () => {
    const c = makeCitation("hybrid", {
      commit_sha: "deadbeef12345678",
      file_path: "crates/rb-query/src/lib.rs",
      line_range: { start: 42, end: 87 },
    });
    const url = buildGitHubUrl("f-crop/rustacean", c);
    expect(url).toBe(
      "https://github.com/f-crop/rustacean/blob/deadbeef12345678/crates/rb-query/src/lib.rs#L42-L87",
    );
  });

  it("uses commit_sha not 'main' in the URL", () => {
    const c = makeCitation("dense", { commit_sha: "cafebabe" });
    const url = buildGitHubUrl("org/repo", c);
    expect(url).toContain("cafebabe");
    expect(url).not.toContain("/main/");
  });
});

// ---------------------------------------------------------------------------
// extractCitationsFromItems — defensive paths
// ---------------------------------------------------------------------------

describe("extractCitationsFromItems", () => {
  it("extracts citations from a JSON-string tool_result", () => {
    const citations = [makeCitation("dense"), makeCitation("sparse")];
    const items = [
      {
        type: "tool_result",
        content: JSON.stringify({ results: [], citations }),
      },
    ];
    const extracted = extractCitationsFromItems(items);
    expect(extracted).toHaveLength(2);
    expect(extracted[0]?.source_kind).toBe("dense");
    expect(extracted[1]?.source_kind).toBe("sparse");
  });

  it("extracts citations from an object tool_result", () => {
    const citations = [makeCitation("rerank")];
    const items = [{ type: "tool_result", content: { citations } }];
    const extracted = extractCitationsFromItems(items);
    expect(extracted).toHaveLength(1);
    expect(extracted[0]?.source_kind).toBe("rerank");
  });

  it("returns [] when tool_result has no citations field", () => {
    const items = [
      { type: "tool_result", content: JSON.stringify({ results: [] }) },
    ];
    expect(extractCitationsFromItems(items)).toHaveLength(0);
  });

  it("returns [] for non-tool_result items", () => {
    const items = [
      { type: "text", content: JSON.stringify({ citations: [makeCitation("hybrid")] }) },
    ];
    expect(extractCitationsFromItems(items)).toHaveLength(0);
  });

  it("returns [] when content is null/undefined", () => {
    const items = [{ type: "tool_result", content: null }];
    expect(extractCitationsFromItems(items)).toHaveLength(0);
  });

  it("returns [] when content is malformed JSON string", () => {
    const items = [{ type: "tool_result", content: "not json {" }];
    expect(extractCitationsFromItems(items)).toHaveLength(0);
  });

  it("filters out malformed citation objects from the array", () => {
    const goodCitation = makeCitation("dense");
    const badCitation = { version: "v1" }; // missing required fields
    const items = [
      { type: "tool_result", content: { citations: [goodCitation, badCitation] } },
    ];
    const extracted = extractCitationsFromItems(items);
    expect(extracted).toHaveLength(1);
    expect(extracted[0]?.source_kind).toBe("dense");
  });

  it("aggregates citations across multiple tool_result items", () => {
    const items = [
      { type: "tool_result", content: { citations: [makeCitation("dense")] } },
      { type: "tool_result", content: { citations: [makeCitation("hybrid")] } },
    ];
    const extracted = extractCitationsFromItems(items);
    expect(extracted).toHaveLength(2);
  });
});
