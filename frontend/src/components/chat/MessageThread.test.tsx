import { describe, it, expect } from "vitest";
import { extractThinkingPhases, buildConsolidatedContent } from "./message-thread-utils";
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

describe("extractThinkingPhases", () => {
  it("returns empty array when there are no thinking items", () => {
    const items: AssistantItem[] = [text("hello", 1), toolUse(2), toolResult(3)];
    expect(extractThinkingPhases(items)).toEqual([]);
  });

  it("returns single thinking content for one thinking block", () => {
    const items: AssistantItem[] = [thinking("first thought", 1), text("answer", 2)];
    expect(extractThinkingPhases(items)).toEqual(["first thought"]);
  });

  it("extracts all thinking phases from interleaved items", () => {
    const items: AssistantItem[] = [
      thinking("phase one", 1),
      toolUse(2),
      toolResult(3),
      thinking("phase two", 4),
      text("final answer", 5),
    ];
    expect(extractThinkingPhases(items)).toEqual(["phase one", "phase two"]);
  });

  it("includes empty thinking content in returned phases", () => {
    const items: AssistantItem[] = [thinking("", 1), thinking("   ", 2)];
    expect(extractThinkingPhases(items)).toEqual(["", "   "]);
  });
});

describe("buildConsolidatedContent", () => {
  it("returns null for empty array (no accordion)", () => {
    expect(buildConsolidatedContent([])).toBeNull();
  });

  it("returns null when all phases are empty or whitespace (no accordion)", () => {
    expect(buildConsolidatedContent(["", "  ", "\n"])).toBeNull();
  });

  it("returns content string for a single non-empty phase (one accordion)", () => {
    const result = buildConsolidatedContent(["some reasoning"]);
    expect(result).toBe("some reasoning");
  });

  it("joins multiple phases with separator for interleaved thinking (one accordion)", () => {
    const result = buildConsolidatedContent(["phase one", "phase two"]);
    expect(result).toBe("phase one\n\n---\n\nphase two");
  });

  it("strips empty phases so separators only appear between substantive content", () => {
    const result = buildConsolidatedContent(["phase one", "", "phase two"]);
    expect(result).toBe("phase one\n\n---\n\nphase two");
  });
});
