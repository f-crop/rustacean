import type { CitationV1 } from "@/types/citations";

export function buildGitHubUrl(repoFullName: string, citation: CitationV1): string {
  const { commit_sha, file_path, line_range } = citation;
  return `https://github.com/${repoFullName}/blob/${commit_sha}/${file_path}#L${line_range.start}-L${line_range.end}`;
}

/** Extract citations from tool_result items inside an assistant turn. */
export function extractCitationsFromItems(
  items: ReadonlyArray<{ type: string; content?: unknown }>,
): readonly CitationV1[] {
  const results: CitationV1[] = [];
  for (const item of items) {
    if (item.type !== "tool_result" || item.content == null) continue;
    const parsed = coerceToCitationList(item.content);
    if (parsed.length > 0) results.push(...parsed);
  }
  return results;
}

function coerceToCitationList(content: unknown): CitationV1[] {
  const obj = typeof content === "string" ? tryParseJson(content) : content;
  if (obj == null || typeof obj !== "object") return [];
  const arr = (obj as Record<string, unknown>).citations;
  if (!Array.isArray(arr)) return [];
  return arr.filter(isCitationV1Like);
}

function isCitationV1Like(v: unknown): v is CitationV1 {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return (
    typeof o.version === "string" &&
    typeof o.repo_id === "string" &&
    typeof o.file_path === "string" &&
    typeof o.commit_sha === "string" &&
    typeof o.score === "number" &&
    typeof o.source_kind === "string" &&
    typeof o.line_range === "object" &&
    o.line_range !== null
  );
}

function tryParseJson(s: string): unknown {
  try {
    return JSON.parse(s);
  } catch {
    return null;
  }
}
