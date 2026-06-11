const TRUNCATE_LINES = 200;
const TRUNCATE_BYTES = 8192;

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
