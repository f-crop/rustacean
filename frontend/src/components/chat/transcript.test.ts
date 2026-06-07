import { describe, it, expect } from "vitest";
import { buildTranscript, buildTranscriptFromHistory } from "./transcript";
import type { StreamedEvent } from "@/hooks/useEventStream";
import type { ChatMessage } from "@/lib/chat-api";

function sseEvent(payload: Record<string, unknown>, sequence: number): StreamedEvent {
  return {
    id: null,
    type: "session.event",
    data: JSON.stringify({ session_id: "s1", event_type: payload.type, sequence, payload }),
  };
}

describe("buildTranscript — turn_complete flush", () => {
  it("flushes pending assistant (inProgress → false) when turn_complete arrives", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "Hi there!" }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const [user, assistant] = items;
    expect(user?.kind).toBe("user");
    expect(assistant?.kind).toBe("assistant");
    // Must NOT be inProgress after turn_complete
    expect((assistant as { kind: string; inProgress?: boolean }).inProgress).toBeUndefined();
  });

  it("keeps inProgress true when turn_complete has not arrived", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "Hi there!" }, 2),
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    expect((assistant as { kind: string; inProgress?: boolean }).inProgress).toBe(true);
  });

  it("turn_complete on empty pending is a no-op (does not add empty assistant)", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 1),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(0);
  });

  it("multi-turn: turn_complete after each reply leaves both turns non-inProgress", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "turn one" }, 1),
      sseEvent({ type: "text", text: "Reply one" }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
      sseEvent({ type: "user_input", text: "turn two" }, 4),
      sseEvent({ type: "text", text: "Reply two" }, 5),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 6),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(4);

    const assistants = items.filter((i) => i.kind === "assistant");
    expect(assistants).toHaveLength(2);
    for (const a of assistants) {
      expect((a as { kind: string; inProgress?: boolean }).inProgress).toBeUndefined();
    }
  });
});

describe("buildTranscript — tool_use turn_complete handling", () => {
  it("does NOT flush on turn_complete with stop_reason=tool_use — keeps tool_use and tool_result in same assistant", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "list files" }, 1),
      sseEvent({ type: "tool_use", id: "tu-1", name: "list_directory", input: { path: "." } }, 2),
      // Intermediate turn_complete emitted by claude when it pauses for tool execution
      sseEvent({ type: "turn_complete", stop_reason: "tool_use" }, 3),
      sseEvent({ type: "tool_result", tool_use_id: "tu-1", content: ["a.rs"], is_error: false }, 4),
      sseEvent({ type: "text", text: "Here are the files." }, 5),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 6),
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const [user, assistant] = items;
    expect(user?.kind).toBe("user");
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("unreachable");

    // All blocks must be in one assistant item
    expect(assistant.items).toHaveLength(3);
    expect(assistant.items[0]?.type).toBe("tool_use");
    expect(assistant.items[1]?.type).toBe("tool_result");
    expect(assistant.items[2]?.type).toBe("text");
    expect(assistant.inProgress).toBeUndefined();
  });

  it("turn_complete(end_turn) still flushes correctly", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "Hi!" }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    expect((assistant as { inProgress?: boolean }).inProgress).toBeUndefined();
  });

  it("in-progress tool_use (no turn_complete yet) shows tool_use as inProgress", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "list files" }, 1),
      sseEvent({ type: "tool_use", id: "tu-1", name: "bash", input: {} }, 2),
    ];

    const items = buildTranscript(events);
    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("unreachable");
    expect(assistant.inProgress).toBe(true);
    expect(assistant.items[0]?.type).toBe("tool_use");
  });
});

describe("buildTranscriptFromHistory — split-batch tool_use merge", () => {
  it("merges consecutive assistant rows when first ends with tool_use", () => {
    const messages: ChatMessage[] = [
      { id: "u1", seq: 1, role: "user", body: "list files", created_at: "2026-01-01T00:00:00Z" },
      {
        id: "a1",
        seq: 2,
        role: "assistant",
        body: JSON.stringify([{ type: "tool_use", id: "tu-1", name: "bash", input: {} }]),
        created_at: "2026-01-01T00:00:01Z",
      },
      {
        id: "a2",
        seq: 3,
        role: "assistant",
        body: JSON.stringify([
          { type: "tool_result", tool_use_id: "tu-1", content: ["a.rs"], is_error: false },
          { type: "text", text: "Here are the files." },
        ]),
        created_at: "2026-01-01T00:00:02Z",
      },
    ];

    const items = buildTranscriptFromHistory(messages);

    // Two items: user + one merged assistant (not three)
    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("unreachable");

    // All three blocks in the single merged assistant
    expect(assistant.items).toHaveLength(3);
    expect(assistant.items[0]?.type).toBe("tool_use");
    expect(assistant.items[1]?.type).toBe("tool_result");
    expect(assistant.items[2]?.type).toBe("text");
    // Preserves the id of the first row
    expect(assistant.id).toBe("a1");
  });

  it("does NOT merge consecutive assistant rows when first does not end with tool_use", () => {
    const messages: ChatMessage[] = [
      { id: "u1", seq: 1, role: "user", body: "hi", created_at: "2026-01-01T00:00:00Z" },
      {
        id: "a1",
        seq: 2,
        role: "assistant",
        body: JSON.stringify([{ type: "text", text: "Hello" }]),
        created_at: "2026-01-01T00:00:01Z",
      },
      {
        id: "a2",
        seq: 3,
        role: "assistant",
        body: JSON.stringify([{ type: "text", text: "How are you?" }]),
        created_at: "2026-01-01T00:00:02Z",
      },
    ];

    const items = buildTranscriptFromHistory(messages);
    // Three items: user + two separate assistant rows
    expect(items).toHaveLength(3);
  });

  it("merges multi-tool-use chains (tool_use → tool_result → tool_use → tool_result → text)", () => {
    const messages: ChatMessage[] = [
      { id: "u1", seq: 1, role: "user", body: "do tasks", created_at: "2026-01-01T00:00:00Z" },
      {
        id: "a1",
        seq: 2,
        role: "assistant",
        body: JSON.stringify([{ type: "tool_use", id: "t1", name: "bash", input: {} }]),
        created_at: "2026-01-01T00:00:01Z",
      },
      {
        id: "a2",
        seq: 3,
        role: "assistant",
        body: JSON.stringify([
          { type: "tool_result", tool_use_id: "t1", content: "done", is_error: false },
          { type: "tool_use", id: "t2", name: "read_file", input: { path: "f" } },
        ]),
        created_at: "2026-01-01T00:00:02Z",
      },
      {
        id: "a3",
        seq: 4,
        role: "assistant",
        body: JSON.stringify([
          { type: "tool_result", tool_use_id: "t2", content: "contents", is_error: false },
          { type: "text", text: "All done." },
        ]),
        created_at: "2026-01-01T00:00:03Z",
      },
    ];

    const items = buildTranscriptFromHistory(messages);
    expect(items).toHaveLength(2);
    const assistant = items[1];
    if (assistant?.kind !== "assistant") throw new Error("unreachable");
    expect(assistant.items).toHaveLength(5);
    expect(assistant.items[0]?.type).toBe("tool_use");
    expect(assistant.items[1]?.type).toBe("tool_result");
    expect(assistant.items[2]?.type).toBe("tool_use");
    expect(assistant.items[3]?.type).toBe("tool_result");
    expect(assistant.items[4]?.type).toBe("text");
  });
});
