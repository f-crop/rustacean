import type { components } from "@/api/generated/schema";

export type CitationV1 = components["schemas"]["CitationV1"];
export type SourceKind = components["schemas"]["SourceKind"];
export type LineRange = components["schemas"]["LineRange"];

const VALID_SOURCE_KINDS: ReadonlySet<string> = new Set(["dense", "sparse", "hybrid", "rerank"]);

function isValidSourceKind(s: unknown): s is SourceKind {
  return typeof s === "string" && VALID_SOURCE_KINDS.has(s);
}

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return v !== null && typeof v === "object" && !Array.isArray(v);
}

export type ParsedCitationItem =
  | { type: "v1"; citation: CitationV1 }
  | { type: "unknown_version"; version: string };

export function parseCitationResult(raw: unknown): ParsedCitationItem[] {
  let arr: unknown[];

  if (raw === null || raw === undefined) return [];

  if (typeof raw === "string") {
    const trimmed = raw.trim();
    if (!trimmed.startsWith("[")) return [];
    try {
      const parsed: unknown = JSON.parse(trimmed);
      if (!Array.isArray(parsed)) return [];
      arr = parsed;
    } catch {
      return [];
    }
  } else if (Array.isArray(raw)) {
    arr = raw;
  } else {
    return [];
  }

  const results: ParsedCitationItem[] = [];

  for (const item of arr) {
    if (!isPlainObject(item)) continue;

    const { version, repo_id, file_path, line_range, commit_sha, score, source_kind } = item;

    if (version !== "v1") {
      // AC5: emit console.warn for unknown versions instead of throwing
      console.warn(
        `[CitationChipsRenderer] Skipping citation with unknown version: "${String(version)}"`,
      );
      results.push({ type: "unknown_version", version: String(version) });
      continue;
    }

    if (typeof file_path !== "string" || typeof repo_id !== "string") continue;

    const lr = isPlainObject(line_range) ? line_range : {};
    const start = typeof lr["start"] === "number" ? lr["start"] : 0;
    const end = typeof lr["end"] === "number" ? lr["end"] : 0;

    results.push({
      type: "v1",
      citation: {
        version: "v1",
        repo_id,
        file_path,
        line_range: { start, end },
        commit_sha: typeof commit_sha === "string" && commit_sha.length > 0 ? commit_sha : "unknown",
        score: typeof score === "number" ? score : 0,
        source_kind: isValidSourceKind(source_kind) ? source_kind : "dense",
      },
    });
  }

  return results;
}

export function buildGitHubUrl(citation: CitationV1, fullName: string): string {
  const { commit_sha, file_path, line_range } = citation;
  return `https://github.com/${fullName}/blob/${commit_sha}/${file_path}#L${line_range.start}-L${line_range.end}`;
}

const SOURCE_KIND_BADGE: Record<SourceKind, string> = {
  dense: "bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300",
  sparse: "bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300",
  hybrid: "bg-purple-100 text-purple-700 dark:bg-purple-900/40 dark:text-purple-300",
  rerank: "bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300",
};

export function sourceKindBadgeClass(kind: SourceKind): string {
  return SOURCE_KIND_BADGE[kind] ?? SOURCE_KIND_BADGE.dense;
}
