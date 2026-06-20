const TRUNCATE_LINES = 200;
const TRUNCATE_BYTES = 8192;

export type ToolRendererType = "read" | "bash" | "json";

export function getToolRenderer(toolName: string): ToolRendererType {
  if (toolName === "Read") return "read";
  if (toolName === "Bash") return "bash";
  return "json";
}

export function getArgPreview(name: string, input: unknown): string {
  if (input === null || input === undefined) return "";
  if (typeof input === "object" && !Array.isArray(input)) {
    const obj = input as Record<string, unknown>;
    if ((name === "Read" || name === "Write" || name === "Edit") && typeof obj["file_path"] === "string") {
      return obj["file_path"] as string;
    }
    if (name === "Bash" && typeof obj["command"] === "string") {
      return obj["command"] as string;
    }
    if (name === "ToolSearch" && typeof obj["query"] === "string") {
      return obj["query"] as string;
    }
    if (name === "Agent" && typeof (obj["prompt"] ?? obj["description"]) === "string") {
      return (obj["prompt"] ?? obj["description"]) as string;
    }
  }
  const raw = JSON.stringify(input);
  return raw.length > 80 ? raw.slice(0, 77) + "…" : raw;
}

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
