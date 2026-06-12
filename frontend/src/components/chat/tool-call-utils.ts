const TRUNCATE_LINES = 200;
const TRUNCATE_BYTES = 8192;

export function deepDecodeJsonStrings(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(deepDecodeJsonStrings);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>).map(([k, v]) => [
        k,
        deepDecodeJsonStrings(v),
      ]),
    );
  }
  if (typeof value === "string") {
    const trimmed = value.trim();
    if (trimmed.startsWith("{") || trimmed.startsWith("[")) {
      try {
        const parsed: unknown = JSON.parse(value);
        if (parsed !== null && typeof parsed === "object") {
          return deepDecodeJsonStrings(parsed);
        }
      } catch {
        // not valid JSON — leave as-is
      }
    }
  }
  return value;
}

export function looksLikeMarkdown(value: string): boolean {
  try {
    JSON.parse(value);
    return false;
  } catch {
    return true;
  }
}

export function needsTruncation(content: string): boolean {
  if (new TextEncoder().encode(content).length > TRUNCATE_BYTES) return true;
  return content.split("\n").length > TRUNCATE_LINES;
}
