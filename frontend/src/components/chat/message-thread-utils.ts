import type { AssistantItem } from "./transcript";

export function extractThinkingPhases(items: ReadonlyArray<AssistantItem>): string[] {
  return items
    .filter((item): item is Extract<AssistantItem, { type: "thinking" }> => item.type === "thinking")
    .map((item) => item.thinking);
}

export function buildConsolidatedContent(phases: string[]): string | null {
  const nonEmpty = phases.filter((p) => p.trim().length > 0);
  return nonEmpty.length > 0 ? nonEmpty.join("\n\n---\n\n") : null;
}
