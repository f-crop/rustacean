import { describe, it, expect } from "vitest";
import { getInlineItems } from "./message-thread-utils";
import type { AssistantItem } from "./transcript";

const text = (t: string, seq: number): AssistantItem => ({ type: "text", text: t, seq });
const thinking = (t: string, seq: number): AssistantItem => ({ type: "thinking", thinking: t, seq });
const toolUse = (seq: number): AssistantItem => ({
  type: "tool_use",
  id: `tu-${seq}`,
  name: "bash",
  input: {},
  seq,
});
const toolResult = (seq: number): AssistantItem => ({
  type: "tool_result",
  toolUseId: `tu-${seq - 1}`,
  content: "ok",
  isError: false,
  seq,
});

describe("getInlineItems — inline chronological rendering (replaces consolidated reasoning)", () => {
  it("excludes tool_result items which are consumed by their paired tool_use", () => {
    const items: AssistantItem[] = [toolUse(1), toolResult(2), text("answer", 3)];
    const inline = getInlineItems(items);
    expect(inline.map((i) => i.type)).toEqual(["tool_use", "text"]);
  });

  it("excludes thinking items (not rendered by UI)", () => {
    const items: AssistantItem[] = [
      thinking("thought", 1),
      toolUse(2),
      toolResult(3),
      text("answer", 4),
    ];
    const inline = getInlineItems(items);
    expect(inline.map((i) => i.type)).toEqual(["tool_use", "text"]);
  });

  it("excludes mid-stream thinking interleaved between tool calls", () => {
    const items: AssistantItem[] = [
      toolUse(1),
      toolResult(2),
      thinking("mid thought", 3),
      toolUse(4),
      toolResult(5),
      text("done", 6),
    ];
    const inline = getInlineItems(items);
    expect(inline.map((i) => i.type)).toEqual(["tool_use", "tool_use", "text"]);
  });

  it("returns empty array for empty input", () => {
    expect(getInlineItems([])).toEqual([]);
  });

  it("returns only text when no thinking or tool calls present", () => {
    const items: AssistantItem[] = [text("hello", 1), text("world", 2)];
    expect(getInlineItems(items).map((i) => i.type)).toEqual(["text", "text"]);
  });

  it("thinking-only items are excluded (not rendered)", () => {
    const items: AssistantItem[] = [thinking("a", 1), thinking("b", 2)];
    expect(getInlineItems(items)).toEqual([]);
  });
});
