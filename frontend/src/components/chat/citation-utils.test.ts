import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import {
  parseCitationResult,
  buildGitHubUrl,
  sourceKindBadgeClass,
  type CitationV1,
} from "./citation-utils";

const BASE: CitationV1 = {
  version: "v1",
  repo_id: "repo-uuid-1",
  file_path: "src/lib.rs",
  line_range: { start: 10, end: 25 },
  commit_sha: "abc123def456",
  score: 0.94,
  source_kind: "hybrid",
};

const makeRaw = (overrides: Partial<Record<string, unknown>> = {}): unknown =>
  JSON.stringify([{ ...BASE, ...overrides }]);

// ── parseCitationResult ────────────────────────────────────────────────────

describe("parseCitationResult — input variety", () => {
  it("returns empty array for null", () => {
    expect(parseCitationResult(null)).toEqual([]);
  });

  it("returns empty array for undefined", () => {
    expect(parseCitationResult(undefined)).toEqual([]);
  });

  it("returns empty array for non-array JSON string", () => {
    expect(parseCitationResult('{"foo":"bar"}')).toEqual([]);
  });

  it("returns empty array for invalid JSON string", () => {
    expect(parseCitationResult("[not json")).toEqual([]);
  });

  it("returns empty array for empty array JSON string", () => {
    expect(parseCitationResult("[]")).toEqual([]);
  });

  it("returns empty array for empty array value", () => {
    expect(parseCitationResult([])).toEqual([]);
  });

  it("returns empty array for a plain object (not array)", () => {
    expect(parseCitationResult({ version: "v1" })).toEqual([]);
  });

  it("parses a valid CitationV1 JSON string", () => {
    const result = parseCitationResult(makeRaw());
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ type: "v1", citation: { ...BASE } });
  });

  it("parses a valid CitationV1 array value (pre-decoded)", () => {
    const result = parseCitationResult([{ ...BASE }]);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ type: "v1" });
  });

  it("skips items that are not plain objects", () => {
    const raw = JSON.stringify([null, 42, "string", { ...BASE }]);
    const result = parseCitationResult(raw);
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ type: "v1" });
  });

  it("skips items with missing file_path", () => {
    const raw = JSON.stringify([{ ...BASE, file_path: undefined }]);
    expect(parseCitationResult(raw)).toEqual([]);
  });

  it("skips items with empty-string file_path", () => {
    const raw = JSON.stringify([{ ...BASE, file_path: "" }]);
    expect(parseCitationResult(raw)).toEqual([]);
  });

  it("skips items with whitespace-only file_path", () => {
    const raw = JSON.stringify([{ ...BASE, file_path: "   " }]);
    expect(parseCitationResult(raw)).toEqual([]);
  });

  it("skips items with missing repo_id", () => {
    const raw = JSON.stringify([{ ...BASE, repo_id: undefined }]);
    expect(parseCitationResult(raw)).toEqual([]);
  });

  it("defaults missing commit_sha to 'unknown'", () => {
    const result = parseCitationResult(JSON.stringify([{ ...BASE, commit_sha: "" }]));
    expect(result[0]).toMatchObject({ type: "v1", citation: { commit_sha: "unknown" } });
  });

  it("defaults missing score to 0", () => {
    const result = parseCitationResult(JSON.stringify([{ ...BASE, score: undefined }]));
    expect(result[0]).toMatchObject({ type: "v1", citation: { score: 0 } });
  });

  it("defaults missing line_range to {start:0, end:0}", () => {
    const result = parseCitationResult(JSON.stringify([{ ...BASE, line_range: undefined }]));
    expect(result[0]).toMatchObject({ type: "v1", citation: { line_range: { start: 0, end: 0 } } });
  });

  it("defaults unrecognized source_kind to 'dense'", () => {
    const result = parseCitationResult(JSON.stringify([{ ...BASE, source_kind: "custom" }]));
    expect(result[0]).toMatchObject({ type: "v1", citation: { source_kind: "dense" } });
  });
});

// ── source_kind variants ───────────────────────────────────────────────────

describe("parseCitationResult — all four source_kind variants", () => {
  for (const kind of ["dense", "sparse", "hybrid", "rerank"] as const) {
    it(`preserves source_kind "${kind}"`, () => {
      const result = parseCitationResult(makeRaw({ source_kind: kind }));
      expect(result[0]).toMatchObject({ type: "v1", citation: { source_kind: kind } });
    });
  }
});

// ── version guard ──────────────────────────────────────────────────────────

describe("parseCitationResult — version guard (AC5)", () => {
  let warnSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});
  });

  afterEach(() => {
    warnSpy.mockRestore();
  });

  it("emits console.warn for unknown version", () => {
    parseCitationResult(makeRaw({ version: "v2" }));
    expect(warnSpy).toHaveBeenCalledWith(
      expect.stringContaining("v2"),
    );
  });

  it("returns unknown_version item instead of dropping silently", () => {
    const result = parseCitationResult(makeRaw({ version: "v99" }));
    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ type: "unknown_version", version: "v99" });
  });

  it("handles mix of v1 and unknown-version citations", () => {
    const raw = JSON.stringify([
      { ...BASE, version: "v2" },
      { ...BASE },
    ]);
    const result = parseCitationResult(raw);
    expect(result).toHaveLength(2);
    expect(result[0]).toMatchObject({ type: "unknown_version" });
    expect(result[1]).toMatchObject({ type: "v1" });
  });
});

// ── buildGitHubUrl ─────────────────────────────────────────────────────────

describe("buildGitHubUrl", () => {
  it("builds a correct GitHub blob URL with line anchor", () => {
    const url = buildGitHubUrl(BASE, "acme/web-app");
    expect(url).toBe(
      "https://github.com/acme/web-app/blob/abc123def456/src/lib.rs#L10-L25",
    );
  });

  it("handles zero-based line range", () => {
    const citation: CitationV1 = { ...BASE, line_range: { start: 0, end: 0 } };
    const url = buildGitHubUrl(citation, "acme/web-app");
    expect(url).toContain("#L0-L0");
  });

  it("encodes the full_name as-is (owner/repo)", () => {
    const url = buildGitHubUrl(BASE, "my-org/my-repo");
    expect(url).toContain("https://github.com/my-org/my-repo/blob/");
  });
});

// ── sourceKindBadgeClass ───────────────────────────────────────────────────

describe("sourceKindBadgeClass — badge colors", () => {
  it("dense → blue classes", () => {
    const cls = sourceKindBadgeClass("dense");
    expect(cls).toContain("blue");
  });

  it("sparse → green classes", () => {
    const cls = sourceKindBadgeClass("sparse");
    expect(cls).toContain("green");
  });

  it("hybrid → purple classes", () => {
    const cls = sourceKindBadgeClass("hybrid");
    expect(cls).toContain("purple");
  });

  it("rerank → amber classes", () => {
    const cls = sourceKindBadgeClass("rerank");
    expect(cls).toContain("amber");
  });
});
