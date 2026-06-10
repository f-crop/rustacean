import { describe, it, expect } from "vitest";
import { buildTranscript, buildTranscriptFromHistory, mergeTranscript } from "./transcript";
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

// ---------------------------------------------------------------------------
// mergeTranscript — identity-based merge (RUSAA-1974 signal-shape matrix)
// ---------------------------------------------------------------------------

const TURN_X = "aaaaaaaa-0000-0000-0000-000000000001";
const TURN_Y = "aaaaaaaa-0000-0000-0000-000000000002";
const TURN_Z = "aaaaaaaa-0000-0000-0000-000000000003";

function v2Event(
  payload: Record<string, unknown>,
  sequence: number,
  turnId?: string,
): StreamedEvent {
  return {
    id: null,
    type: "session.event",
    data: JSON.stringify({
      session_id: "s1",
      event_type: String(payload.type),
      sequence,
      payload,
      ...(turnId ? { turn_id: turnId, protocol_version: 2 } : {}),
    }),
  };
}

function histMsg(
  id: string,
  seq: number,
  role: "user" | "assistant",
  body: string,
  turnId?: string,
): ChatMessage {
  return {
    id,
    seq,
    role,
    body,
    created_at: "2026-06-10T00:00:00Z",
    ...(turnId ? { turn_id: turnId } : {}),
  };
}

describe("mergeTranscript — signal shape 1: no history, v2 SSE starts streaming", () => {
  it("shows in-progress assistant from SSE before DB persists", () => {
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "hello" }, 1, TURN_X),
      v2Event({ type: "text", text: "Hi there!" }, 2, TURN_X),
    ];
    const items = mergeTranscript([], events, [{ id: "p1", text: "hello" }]);

    // pending bubble is covered by SSE user_input echo
    expect(items).toHaveLength(1);
    const [a1] = items;
    expect(a1?.kind).toBe("assistant");
    if (a1?.kind !== "assistant") throw new Error("unreachable");
    expect(a1.inProgress).toBe(true);
    expect(a1.items[0]).toMatchObject({ type: "text", text: "Hi there!" });
  });
});

describe("mergeTranscript — signal shape 2: reload from history only (no live SSE)", () => {
  it("renders two completed turns from DB with turn_id", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "hello", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([{ type: "text", text: "Hi!" }]), TURN_X),
      histMsg("u2", 3, "user", "bye", TURN_Y),
      histMsg("a2", 4, "assistant", JSON.stringify([{ type: "text", text: "Goodbye." }]), TURN_Y),
    ];
    const items = mergeTranscript(hist, []);

    expect(items).toHaveLength(4);
    expect(items[0]?.kind).toBe("user");
    expect(items[1]?.kind).toBe("assistant");
    expect(items[2]?.kind).toBe("user");
    expect(items[3]?.kind).toBe("assistant");
    if (items[1]?.kind !== "assistant") throw new Error("unreachable");
    expect(items[1].inProgress).toBeUndefined();
  });
});

describe("mergeTranscript — signal shape 3: reconnect mid-stream", () => {
  it("replaces completed DB turn with live in-progress content for the new turn", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "q1", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([{ type: "text", text: "Reply 1" }]), TURN_X),
      histMsg("u2", 3, "user", "q2", TURN_Y),
    ];
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "q1" }, 1, TURN_X),
      v2Event({ type: "text", text: "Reply 1" }, 2, TURN_X),
      v2Event({ type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_X),
      v2Event({ type: "user_input", text: "q2" }, 4, TURN_Y),
      v2Event({ type: "text", text: "Streaming..." }, 5, TURN_Y),
    ];
    const items = mergeTranscript(hist, events);

    expect(items).toHaveLength(4); // u1, a1, u2, a2-live
    expect(items[0]).toMatchObject({ kind: "user", text: "q1" });
    expect(items[1]).toMatchObject({ kind: "assistant" });
    // DB content wins for TURN_X (completed)
    if (items[1]?.kind !== "assistant") throw new Error("unreachable");
    expect(items[1].inProgress).toBeUndefined();
    expect(items[2]).toMatchObject({ kind: "user", text: "q2" });
    // TURN_Y not yet in DB → appended from live
    const a2 = items[3];
    expect(a2?.kind).toBe("assistant");
    if (a2?.kind !== "assistant") throw new Error("unreachable");
    expect(a2.inProgress).toBe(true);
    expect(a2.items[0]).toMatchObject({ type: "text", text: "Streaming..." });
  });
});

describe("mergeTranscript — signal shape 4: CLI restart (no user_input in SSE, v2 turn_id)", () => {
  it("recognises replayed completed turn and shows new in-progress via turn_id", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "q1", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([{ type: "text", text: "Reply 1" }]), TURN_X),
      histMsg("u2", 3, "user", "q2", TURN_Y),
    ];
    // CLI restart: no user_input events; TURN_X replay then TURN_Y in-progress
    const events: StreamedEvent[] = [
      v2Event({ type: "text", text: "Reply 1" }, 2, TURN_X),
      v2Event({ type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_X),
      v2Event({ type: "text", text: "Answer 2..." }, 4, TURN_Y),
    ];
    const items = mergeTranscript(hist, events);

    expect(items).toHaveLength(4); // u1, a1, u2, a2-live
    // a1 from DB (TURN_X is complete)
    if (items[1]?.kind !== "assistant") throw new Error("unreachable");
    expect(items[1].inProgress).toBeUndefined();
    // a2-live from SSE (TURN_Y not in DB yet)
    const a2 = items[3];
    expect(a2?.kind).toBe("assistant");
    if (a2?.kind !== "assistant") throw new Error("unreachable");
    expect(a2.inProgress).toBe(true);
  });
});

describe("mergeTranscript — signal shape 5: turn_complete unlocks; isStreaming → false", () => {
  it("completed assistant has no inProgress flag after turn_complete", () => {
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "hello" }, 1, TURN_X),
      v2Event({ type: "text", text: "Done." }, 2, TURN_X),
      v2Event({ type: "turn_complete", stop_reason: "end_turn" }, 3, TURN_X),
    ];
    const items = mergeTranscript([], events);

    // Only the in-progress assistant is rendered (user_input not yet in DB)
    expect(items).toHaveLength(1);
    const [a] = items;
    expect(a?.kind).toBe("assistant");
    if (a?.kind !== "assistant") throw new Error("unreachable");
    expect(a.inProgress).toBeUndefined();
  });
});

describe("mergeTranscript — signal shape 6: split-batch assistant rows sharing turn_id", () => {
  it("merges two DB rows with the same turn_id into one AssistantTranscriptItem", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "search", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([
        { type: "tool_use", id: "tu1", name: "mcp__search", input: {} },
      ]), TURN_X),
      histMsg("a2", 3, "assistant", JSON.stringify([
        { type: "tool_result", tool_use_id: "tu1", content: "results", is_error: false },
        { type: "text", text: "Found it." },
      ]), TURN_X),
    ];
    const items = mergeTranscript(hist, []);

    expect(items).toHaveLength(2); // user + merged assistant
    const a = items[1];
    expect(a?.kind).toBe("assistant");
    if (a?.kind !== "assistant") throw new Error("unreachable");
    expect(a.items).toHaveLength(3); // tool_use + tool_result + text
  });
});

describe("mergeTranscript — signal shape 7: backward-compat for NULL turn_id history", () => {
  it("renders legacy v1 rows without turn_id using positional logic", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "hello"),  // no turn_id
      histMsg("a1", 2, "assistant", "Hi there!"), // no turn_id, plain text body
    ];
    const items = mergeTranscript(hist, []);

    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ kind: "user", text: "hello" });
    const a = items[1];
    expect(a?.kind).toBe("assistant");
    if (a?.kind !== "assistant") throw new Error("unreachable");
    expect(a.items[0]).toMatchObject({ type: "text", text: "Hi there!" });
    expect(a.inProgress).toBeUndefined();
  });

  it("merges split-batch v1 rows (tool_use→tool_result) positionally", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "q"),
      histMsg("a1", 2, "assistant", JSON.stringify([
        { type: "tool_use", id: "t1", name: "fn", input: {} },
      ])), // no turn_id
      histMsg("a2", 3, "assistant", JSON.stringify([
        { type: "tool_result", tool_use_id: "t1", content: "res", is_error: false },
      ])), // no turn_id
    ];
    const items = mergeTranscript(hist, []);

    expect(items).toHaveLength(2); // user + merged assistant
    const a = items[1];
    expect(a?.kind).toBe("assistant");
    if (a?.kind !== "assistant") throw new Error("unreachable");
    expect(a.items).toHaveLength(2);
  });
});

describe("mergeTranscript — signal shape 8: pending queue + SSE echo coverage", () => {
  it("removes optimistic bubble once SSE user_input echo arrives", () => {
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "hello" }, 1, TURN_X),
      v2Event({ type: "text", text: "Response..." }, 2, TURN_X),
    ];
    const items = mergeTranscript([], events, [{ id: "p1", text: "hello" }]);

    // Pending bubble should be filtered out (covered by SSE echo)
    const userItems = items.filter((i) => i.kind === "user");
    expect(userItems).toHaveLength(0);
    // Assistant is in-progress
    expect(items).toHaveLength(1);
    expect(items[0]?.kind).toBe("assistant");
  });

  it("shows optimistic bubble when SSE has no echo yet", () => {
    const items = mergeTranscript([], [], [{ id: "p1", text: "hello" }]);

    expect(items).toHaveLength(1);
    expect(items[0]).toMatchObject({ kind: "user", text: "hello" });
  });

  it("removes optimistic bubble once DB history has the user row", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "hello", TURN_X),
    ];
    const items = mergeTranscript(hist, [], [{ id: "p1", text: "hello" }]);

    const userItems = items.filter((i) => i.kind === "user");
    expect(userItems).toHaveLength(1); // only the DB row, not a duplicate
  });
});

describe("mergeTranscript — signal shape 9: multi-turn v2 + live in-progress assistant supersedes DB", () => {
  it("live in-progress content supersedes DB for the same turn_id", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "q", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([{ type: "text", text: "old DB" }]), TURN_X),
    ];
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "q" }, 1, TURN_X),
      v2Event({ type: "text", text: "live streaming..." }, 2, TURN_X),
    ];
    const items = mergeTranscript(hist, events);

    expect(items).toHaveLength(2);
    const a = items[1];
    expect(a?.kind).toBe("assistant");
    if (a?.kind !== "assistant") throw new Error("unreachable");
    // Live content takes priority when in-progress
    expect(a.inProgress).toBe(true);
    expect(a.items[0]).toMatchObject({ type: "text", text: "live streaming..." });
  });
});

describe("mergeTranscript — signal shape 10: three-turn v2 all from DB, no live SSE", () => {
  it("renders three turns correctly ordered", () => {
    const hist: ChatMessage[] = [
      histMsg("u1", 1, "user", "q1", TURN_X),
      histMsg("a1", 2, "assistant", JSON.stringify([{ type: "text", text: "a1" }]), TURN_X),
      histMsg("u2", 3, "user", "q2", TURN_Y),
      histMsg("a2", 4, "assistant", JSON.stringify([{ type: "text", text: "a2" }]), TURN_Y),
      histMsg("u3", 5, "user", "q3", TURN_Z),
      histMsg("a3", 6, "assistant", JSON.stringify([{ type: "text", text: "a3" }]), TURN_Z),
    ];
    const items = mergeTranscript(hist, []);

    expect(items).toHaveLength(6);
    const texts = items
      .filter((i) => i.kind === "assistant")
      .flatMap((i) => (i.kind === "assistant" ? i.items : []))
      .filter((i) => i.type === "text")
      .map((i) => (i.type === "text" ? i.text : ""));
    expect(texts).toEqual(["a1", "a2", "a3"]);
  });
});
