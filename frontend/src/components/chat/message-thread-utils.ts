import type { AssistantItem } from "./transcript";

/** Returns items that render as inline blocks — excludes tool_result entries (consumed by their paired tool_use). */
export function getInlineItems(items: ReadonlyArray<AssistantItem>): ReadonlyArray<AssistantItem> {
  return items.filter((item) => item.type !== "tool_result");
}
