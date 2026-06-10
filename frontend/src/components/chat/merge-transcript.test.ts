import { describe, it, expect } from "vitest";
import { mergeTranscript } from "./merge-transcript";
import type { StreamedEvent } from "@/hooks/useEventStream";
import type { ChatMessage } from "@/lib/chat-api";

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
  it("shows user bubble and in-progress assistant from SSE before DB persists", () => {
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "hello" }, 1, TURN_X),
      v2Event({ type: "text", text: "Hi there!" }, 2, TURN_X),
    ];
    const items = mergeTranscript([], events, [{ id: "p1", text: "hello" }]);

    // SSE user_input → live user bubble (pending send suppressed by coveredTexts)
    expect(items).toHaveLength(2);
    const [u1, a1] = items;
    expect(u1?.kind).toBe("user");
    if (u1?.kind !== "user") throw new Error("unreachable");
    expect(u1.text).toBe("hello");
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

    // u2 from DB suppresses the live user bubble for TURN_Y (processedUserTurnIds).
    expect(items).toHaveLength(4); // u1, a1, u2(DB), a2-live
    expect(items[0]).toMatchObject({ kind: "user", text: "q1" });
    expect(items[1]).toMatchObject({ kind: "assistant" });
    // DB content wins for TURN_X (completed)
    if (items[1]?.kind !== "assistant") throw new Error("unreachable");
    expect(items[1].inProgress).toBeUndefined();
    expect(items[2]).toMatchObject({ kind: "user", text: "q2" });
    // TURN_Y not yet in DB → appended from live (no duplicate user bubble because u2 in DB)
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

    // user_input → live user bubble + completed assistant (DB not yet populated)
    expect(items).toHaveLength(2);
    const [u, a] = items;
    expect(u?.kind).toBe("user");
    if (u?.kind !== "user") throw new Error("unreachable");
    expect(u.text).toBe("hello");
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
  it("deduplicates: pending send suppressed, live user bubble shown (exactly one user bubble)", () => {
    const events: StreamedEvent[] = [
      v2Event({ type: "user_input", text: "hello" }, 1, TURN_X),
      v2Event({ type: "text", text: "Response..." }, 2, TURN_X),
    ];
    const items = mergeTranscript([], events, [{ id: "p1", text: "hello" }]);

    // SSE echo adds a live user bubble; pending send is suppressed (covered by live bubble).
    // Net: exactly one "hello" user bubble.
    const userItems = items.filter((i) => i.kind === "user");
    expect(userItems).toHaveLength(1);
    expect(userItems[0]).toMatchObject({ kind: "user", text: "hello" });
    // Assistant is in-progress
    expect(items).toHaveLength(2);
    expect(items[1]?.kind).toBe("assistant");
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
