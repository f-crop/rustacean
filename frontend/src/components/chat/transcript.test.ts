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

  // RUSAA-1966: text row emitted before tool_use (agent-runner flushed text in one batch
  // and tool events in the next). The split-batch merge must cover text→tool_use splits,
  // not only tool_use→tool_result splits, so dedupeAssistantsPerSegment never sees 2
  // completed items and incorrectly drops the text item.
  it("RUSAA-1966: merges text row followed by tool_use row into single assistant", () => {
    const messages: ChatMessage[] = [
      { id: "u1", seq: 1, role: "user", body: "search rust", created_at: "2026-01-01T00:00:00Z" },
      {
        id: "a1",
        seq: 5,
        role: "assistant",
        body: JSON.stringify([{ type: "text", text: "RustBrain is a code intelligence platform." }]),
        created_at: "2026-01-01T00:00:01Z",
      },
      {
        id: "a2",
        seq: 7,
        role: "assistant",
        body: JSON.stringify([
          { type: "tool_use", id: "tu-1", name: "mcp__rust_brain__search_items", input: { q: "rust" } },
          { type: "tool_result", tool_use_id: "tu-1", content: "3 items found", is_error: false },
        ]),
        created_at: "2026-01-01T00:00:02Z",
      },
    ];

    const items = buildTranscriptFromHistory(messages);

    // Must produce user + ONE merged assistant (not three items)
    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("unreachable");

    // text, tool_use, tool_result all in the single merged item
    expect(assistant.items).toHaveLength(3);
    expect(assistant.items[0]?.type).toBe("text");
    expect(assistant.items[1]?.type).toBe("tool_use");
    expect(assistant.items[2]?.type).toBe("tool_result");
    // Preserves the id and startSeq of the first row
    expect(assistant.id).toBe("a1");
  });

  it("RUSAA-1966: text→text (no tool_use) still produces two separate assistants", () => {
    // Two consecutive text-only rows should NOT be merged
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
    // Two separate assistants — no merge (no tool_use)
    expect(items).toHaveLength(3);
  });
});

describe("buildTranscript — text-before-tool_use merge (RUSAA-1966)", () => {
  it("merges text+end_turn+tool_use into one assistant when end_turn fires before tool_use", () => {
    // Scenario: model emits text (end_turn), CLI then invokes tool_use in next agentic step.
    // buildTranscript must merge the two flushed items so dedupeAssistantsPerSegment sees
    // a single completed item and does not drop the text block.
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "search for rust" }, 1),
      sseEvent({ type: "text", text: "RustBrain is a code intelligence platform." }, 2),
      // end_turn fires here (text-only model response); tool call follows as next step
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
      sseEvent({ type: "tool_use", id: "tu-1", name: "mcp__rust_brain__search_items", input: { q: "rust" } }, 4),
      sseEvent({ type: "turn_complete", stop_reason: "tool_use" }, 5),
      sseEvent({ type: "tool_result", tool_use_id: "tu-1", content: "3 results", is_error: false }, 6),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 7),
    ];

    const items = buildTranscript(events);

    // Must be [user, ONE assistant] — the text and tool blocks merged
    expect(items).toHaveLength(2);
    const assistant = items[1];
    expect(assistant?.kind).toBe("assistant");
    if (assistant?.kind !== "assistant") throw new Error("unreachable");

    expect(assistant.items).toHaveLength(3);
    expect(assistant.items[0]?.type).toBe("text");
    expect(assistant.items[0]?.type === "text" ? assistant.items[0].text : "").toBe(
      "RustBrain is a code intelligence platform.",
    );
    expect(assistant.items[1]?.type).toBe("tool_use");
    expect(assistant.items[2]?.type).toBe("tool_result");
    expect(assistant.inProgress).toBeUndefined();
  });

  it("merges text+end_turn+tool_use(in-progress) into one inProgress assistant", () => {
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "search for rust" }, 1),
      sseEvent({ type: "text", text: "RustBrain is a code intelligence platform." }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
      sseEvent({ type: "tool_use", id: "tu-1", name: "mcp__rust_brain__search_items", input: {} }, 4),
      // no turn_complete yet — tool still in progress
    ];

    const items = buildTranscript(events);

    expect(items).toHaveLength(2);
    const assistant = items[1];
    if (assistant?.kind !== "assistant") throw new Error("unreachable");

    // text merged with the in-progress tool_use
    expect(assistant.items).toHaveLength(2);
    expect(assistant.items[0]?.type).toBe("text");
    expect(assistant.items[1]?.type).toBe("tool_use");
    expect(assistant.inProgress).toBe(true);
  });

  it("does NOT merge when second assistant starts with text (not tool_use)", () => {
    // Two consecutive completed text-only assistants should remain separate
    const events: StreamedEvent[] = [
      sseEvent({ type: "user_input", text: "hello" }, 1),
      sseEvent({ type: "text", text: "First response." }, 2),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 3),
      sseEvent({ type: "text", text: "Second response." }, 4),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 5),
    ];

    const items = buildTranscript(events);

    // [user, asst1, asst2] — two separate assistants
    expect(items).toHaveLength(3);
    const [, a1, a2] = items;
    expect(a1?.kind).toBe("assistant");
    expect(a2?.kind).toBe("assistant");
  });

  it("CLI restart SSE (no user_input): does NOT merge adjacent turns even when second starts with tool_use", () => {
    // Regression guard for RUSAA-1962: CLI restart replays prior turns without
    // user_input events. Two consecutive assistant items from different turns must
    // NOT be merged even if the second starts with tool_use. The !firstLiveUser path
    // in ChatPage handles transcript merging via buildTranscriptFromHistory.
    const events: StreamedEvent[] = [
      // Turn 1 replay: text only (no user_input)
      sseEvent({ type: "text", text: "Turn 1 answer." }, 1),
      sseEvent({ type: "turn_complete", stop_reason: "end_turn" }, 2),
      // Turn 2 streaming: starts with tool_use (genuinely different turn)
      sseEvent({ type: "tool_use", id: "tu-1", name: "search", input: {} }, 3),
      sseEvent({ type: "turn_complete", stop_reason: "tool_use" }, 4),
    ];

    const items = buildTranscript(events);

    // Two separate assistants — no merge
    expect(items).toHaveLength(2);
    const [a1, a2] = items;
    expect(a1?.kind).toBe("assistant");
    if (a1?.kind !== "assistant") throw new Error("unreachable");
    expect(a1.items[0]?.type).toBe("text");

    expect(a2?.kind).toBe("assistant");
    if (a2?.kind !== "assistant") throw new Error("unreachable");
    expect(a2.items[0]?.type).toBe("tool_use");
    expect(a2.inProgress).toBe(true);
  });
});

